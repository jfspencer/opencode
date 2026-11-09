//! Forms (packages/core/src/form.ts): a location's questions to the user, answered or cancelled through the session
//! form endpoints. The question tool asks through them (`Service::ask`), as do clients (`session.form.create`).
//!
//! A form is created pending (`form.created`); a valid answer settles it (`form.replied`), a cancellation too
//! (`form.cancelled`, with an optional message for the asker). Settled forms are kept for ten minutes (`RETENTION`),
//! pending ones until they settle. Like TS's `Cache`, the forms are kept in recency order: every lookup of a form
//! (get, state, reply, cancel, a create naming its ID) moves it to the end, which is the order `list` reports.
//! Shutting the location down (`close`) cancels every pending form, and a form created after that is cancelled as
//! soon as it is announced.

mod date;
mod formats;
mod validate;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use opencode_protocol::Json;
use tokio::sync::oneshot;

use crate::bus::{Bus, LocationRef, PublishOptions};
use crate::json::{self, Obj};

pub use validate::{validate_answer, validate_fields};

/// How long a settled form stays readable.
const RETENTION: Duration = Duration::from_secs(10 * 60);

/// `Form.State`.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Pending,
    /// The answer (`Form.Answer`, a record of values).
    Answered(Json),
    /// Why nobody answered, when the canceller said.
    Cancelled(Option<String>),
}

impl State {
    pub fn to_json(&self) -> Json {
        let mut o = Obj::new();
        match self {
            State::Pending => {
                o.str("status", "pending");
            }
            State::Answered(answer) => {
                o.str("status", "answered").set("answer", answer.clone());
            }
            State::Cancelled(message) => {
                o.str("status", "cancelled").opt_str("message", message.as_deref());
            }
        }
        o.done()
    }

    fn pending(&self) -> bool {
        matches!(self, State::Pending)
    }
}

/// `Form.CreateInput`.
#[derive(Debug, Clone)]
pub struct CreateInput {
    pub id: Option<String>,
    /// The owning session (or `global`, for MCP elicitations without one).
    pub session_id: String,
    pub title: String,
    pub metadata: Option<Json>,
    /// `Form.Fields`: a non-empty array of `Form.Field`.
    pub fields: Json,
}

/// Why a form could not be created.
#[derive(Debug, Clone)]
pub enum CreateError {
    /// `Form.AlreadyExistsError`.
    AlreadyExists(String),
    /// `Form.InvalidFormError`.
    Invalid(String),
}

impl CreateError {
    pub fn message(&self) -> String {
        match self {
            CreateError::AlreadyExists(id) => format!("Form already exists: {id}"),
            CreateError::Invalid(message) => message.clone(),
        }
    }
}

/// Why a reply or cancellation was refused.
#[derive(Debug, Clone)]
pub enum SettleError {
    /// `Form.NotFoundError`.
    NotFound(String),
    /// `Form.AlreadySettledError`.
    AlreadySettled(String),
    /// `Form.InvalidAnswerError` (`id`, `message`).
    InvalidAnswer(String, String),
}

/// `Form.NotFoundError`.
#[derive(Debug, Clone)]
pub struct NotFound(pub String);

struct Entry {
    /// `Form.Info` as created.
    form: Json,
    session_id: String,
    state: State,
    /// When a settled form is forgotten; pending forms are kept.
    expires: Option<Instant>,
    /// The askers waiting for the form to settle.
    waiters: Vec<oneshot::Sender<State>>,
}

/// A location's forms (`Form.Service`).
#[derive(Default)]
pub struct Service {
    entries: Mutex<IndexMap<String, Entry>>,
    closed: AtomicBool,
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Service").field("pending", &self.list(None).len()).finish()
    }
}

fn options(location: &LocationRef) -> PublishOptions {
    PublishOptions { id: None, location: Some(location.clone()) }
}

impl Service {
    /// `requireEntry` (`Cache.getSuccess`): a live entry, moved to the end of the recency order.
    fn touch<'a>(entries: &'a mut IndexMap<String, Entry>, id: &str) -> Option<&'a mut Entry> {
        let index = entries.get_index_of(id)?;
        if entries[index].expires.is_some_and(|at| at <= Instant::now()) {
            entries.shift_remove_index(index);
            return None;
        }
        let last = entries.len() - 1;
        entries.move_index(index, last);
        entries.get_index_mut(last).map(|(_, entry)| entry)
    }

    /// `Form.create`: validates the fields, records the form pending and announces it.
    pub fn create(&self, bus: &Bus, location: &LocationRef, input: CreateInput) -> Result<Json, CreateError> {
        let id = input.id.unwrap_or_else(|| format!("frm_{}", crate::id::ascending()));
        let form = {
            let mut entries = self.entries.lock().unwrap();
            if Self::touch(&mut entries, &id).is_some() {
                return Err(CreateError::AlreadyExists(id));
            }
            if let Some(invalid) = validate_fields(json::array(&input.fields).unwrap_or_default()) {
                return Err(CreateError::Invalid(invalid));
            }
            let mut o = Obj::new();
            o.str("id", &id).str("sessionID", &input.session_id).str("title", &input.title);
            o.opt("metadata", input.metadata);
            o.set("fields", input.fields);
            let form = o.done();
            entries.insert(
                id.clone(),
                Entry {
                    form: form.clone(),
                    session_id: input.session_id,
                    state: State::Pending,
                    expires: None,
                    waiters: Vec::new(),
                },
            );
            form
        };
        let mut data = Obj::new();
        data.set("form", form.clone());
        bus.emit("form.created", data.done(), options(location));
        if self.closed.load(Ordering::SeqCst) {
            let _ = self.cancel(bus, location, &id, None);
        }
        Ok(form)
    }

    /// `Form.ask`: creates the form and waits for it to settle. A wait that is dropped (the call interrupted) cancels
    /// the form.
    pub async fn ask(&self, bus: &Bus, location: &LocationRef, input: CreateInput) -> Result<State, CreateError> {
        let form = self.create(bus, location, input)?;
        let id = json::get_str(&form, "id").unwrap_or_default().to_owned();
        let receiver = {
            let mut entries = self.entries.lock().unwrap();
            let Some(entry) = Self::touch(&mut entries, &id) else {
                // Settled and forgotten already: TS dies here (`Effect.orDie`).
                return Ok(State::Cancelled(None));
            };
            if !entry.state.pending() {
                return Ok(entry.state.clone());
            }
            let (sender, receiver) = oneshot::channel();
            entry.waiters.push(sender);
            receiver
        };
        let mut guard = CancelOnDrop { service: self, bus, location, id, armed: true };
        let state = receiver.await.unwrap_or(State::Cancelled(None));
        guard.armed = false;
        Ok(state)
    }

    /// `Form.get`.
    pub fn get(&self, id: &str) -> Result<Json, NotFound> {
        let mut entries = self.entries.lock().unwrap();
        Self::touch(&mut entries, id).map(|entry| entry.form.clone()).ok_or_else(|| NotFound(id.to_owned()))
    }

    /// `Form.state`.
    pub fn state(&self, id: &str) -> Result<State, NotFound> {
        let mut entries = self.entries.lock().unwrap();
        Self::touch(&mut entries, id).map(|entry| entry.state.clone()).ok_or_else(|| NotFound(id.to_owned()))
    }

    /// `Form.list`: the pending forms (of one owner, when given), in recency order.
    pub fn list(&self, session_id: Option<&str>) -> Vec<Json> {
        let mut entries = self.entries.lock().unwrap();
        let now = Instant::now();
        entries.retain(|_, entry| entry.expires.is_none_or(|at| at > now));
        entries
            .values()
            .filter(|entry| entry.state.pending())
            .filter(|entry| session_id.is_none_or(|id| entry.session_id == id))
            .map(|entry| entry.form.clone())
            .collect()
    }

    /// Settles a pending form: announces it, records the state, then wakes the askers.
    fn settle(
        &self,
        bus: &Bus,
        location: &LocationRef,
        id: &str,
        validate: impl FnOnce(&Entry) -> Result<(&'static str, Json, State), SettleError>,
    ) -> Result<(), SettleError> {
        let mut entries = self.entries.lock().unwrap();
        let Some(entry) = Self::touch(&mut entries, id) else { return Err(SettleError::NotFound(id.to_owned())) };
        if !entry.state.pending() {
            return Err(SettleError::AlreadySettled(id.to_owned()));
        }
        let (ty, data, next) = validate(entry)?;
        bus.emit(ty, data, options(location));
        entry.state = next.clone();
        entry.expires = Some(Instant::now() + RETENTION);
        for waiter in entry.waiters.drain(..) {
            let _ = waiter.send(next.clone());
        }
        Ok(())
    }

    /// `Form.reply`: a valid answer settles the form.
    pub fn reply(&self, bus: &Bus, location: &LocationRef, id: &str, answer: Json) -> Result<(), SettleError> {
        self.settle(bus, location, id, |entry| {
            let fields = json::get(&entry.form, "fields").and_then(json::array).unwrap_or_default();
            if let Some(invalid) = validate_answer(fields, &answer) {
                return Err(SettleError::InvalidAnswer(id.to_owned(), invalid));
            }
            let mut data = Obj::new();
            data.str("id", id).str("sessionID", &entry.session_id).set("answer", answer.clone());
            Ok(("form.replied", data.done(), State::Answered(answer)))
        })
    }

    /// `Form.cancel`.
    pub fn cancel(&self, bus: &Bus, location: &LocationRef, id: &str, message: Option<String>) -> Result<(), SettleError> {
        self.settle(bus, location, id, |entry| {
            let mut data = Obj::new();
            data.str("id", id).str("sessionID", &entry.session_id);
            Ok(("form.cancelled", data.done(), State::Cancelled(message)))
        })
    }

    /// The location shuts down: every pending form is cancelled, and later ones as soon as they are created.
    pub fn close(&self, bus: &Bus, location: &LocationRef) {
        self.closed.store(true, Ordering::SeqCst);
        let pending: Vec<String> = {
            let mut entries = self.entries.lock().unwrap();
            let now = Instant::now();
            entries.retain(|_, entry| entry.expires.is_none_or(|at| at > now));
            entries.iter().filter(|(_, entry)| entry.state.pending()).map(|(id, _)| id.clone()).collect()
        };
        for id in pending {
            let _ = self.cancel(bus, location, &id, None);
        }
    }
}

/// Cancels an asked form whose wait was dropped (`Effect.onInterrupt(() => cancel(form.id))`).
struct CancelOnDrop<'a> {
    service: &'a Service,
    bus: &'a Bus,
    location: &'a LocationRef,
    id: String,
    armed: bool,
}

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.service.cancel(self.bus, self.location, &self.id, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(id: &str) -> CreateInput {
        CreateInput {
            id: Some(id.into()),
            session_id: "ses_a".into(),
            title: "t".into(),
            metadata: None,
            fields: json::parse(r#"[{"key":"k","type":"boolean"}]"#).unwrap(),
        }
    }

    #[test]
    fn recency_order() {
        let bus = Bus::new();
        let location = LocationRef::directory("/x");
        let forms = Service::default();
        forms.create(&bus, &location, input("frm_a")).unwrap();
        forms.create(&bus, &location, input("frm_b")).unwrap();
        let ids = |forms: &Service| {
            forms.list(None).iter().map(|f| json::get_str(f, "id").unwrap().to_owned()).collect::<Vec<_>>()
        };
        assert_eq!(ids(&forms), ["frm_a", "frm_b"]);
        forms.get("frm_a").unwrap();
        assert_eq!(ids(&forms), ["frm_b", "frm_a"]);
        assert!(matches!(forms.create(&bus, &location, input("frm_b")), Err(CreateError::AlreadyExists(_))));
        assert_eq!(ids(&forms), ["frm_a", "frm_b"]);
        forms.cancel(&bus, &location, "frm_b", Some("no".into())).unwrap();
        assert_eq!(ids(&forms), ["frm_a"]);
        assert_eq!(forms.state("frm_b").unwrap(), State::Cancelled(Some("no".into())));
        assert!(matches!(forms.cancel(&bus, &location, "frm_b", None), Err(SettleError::AlreadySettled(_))));
    }
}
