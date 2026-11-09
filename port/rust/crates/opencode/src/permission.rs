//! Permission requests (packages/core/src/permission.ts, permission/saved.ts): a location's pending requests, the
//! rules that settle them, and the project's saved approvals.
//!
//! # Interface (for the tools)
//!
//! A tool checks its work with [`crate::tool::Context::assert`] (`permission.assert({...input,
//! ...Permission.invocation(context)})`): the context fills in the session, the agent and the tool call as the
//! request's source. It returns `Ok(())` when the rules allow the work, or once the user approves it; otherwise a
//! [`crate::tool::Failure`] the tool returns as is (`?`): a denial by the rules or a rejection with feedback is a
//! tool error of type `permission.rejected`, a plain rejection declines the call (the turn stops). `assert_not_denied`
//! only fails on a denial and never asks; `allows` only reports whether the rules already allow it.
//!
//! Requests are evaluated against the session's agent's ruleset (a deny-all ruleset when the agent is unknown),
//! then the session's own rules, then (for what is not denied) the project's saved approvals, and the managed
//! policies' `tool.use` statements (config/plugin/policy.ts's `permission.evaluate` hook). A request with any
//! resource that evaluates to `ask` is published (`permission.asked`) and waits for [`Service::reply`].
//!
//! A requester's `unattended` rules (`Permission.Unattended`) settle what would ask: allowed when they allow every
//! resource, else denied as needing approval nobody is present to give.

// The interface is the tools' (built concurrently): allow it to be unused until then.
#![allow(dead_code)]

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use opencode_protocol::Json;
use tokio::sync::oneshot;

use crate::app::{App, fatal};
use crate::bus::{Bus, LocationRef, PublishOptions};
use crate::http::Failure as HttpFailure;
use crate::inventory::wildcard::wildcard_match;
use crate::json::{self, Obj};
use crate::location::Location;

/// What a tool asks for (`AssertInput` without the requester fields [`crate::tool::Context`] supplies).
#[derive(Debug, Clone, Default)]
pub struct Ask {
    pub action: String,
    pub resources: Vec<String>,
    /// The resources an "always" reply saves as project approvals.
    pub save: Option<Vec<String>>,
    /// Shown with the request (keys whose value is absent are left out).
    pub metadata: Option<Json>,
}

/// A request's requester (`AssertInput`'s `sessionID`, `agent`, `source`, and an optional preset `id`).
#[derive(Debug, Clone, Default)]
pub struct Requester {
    pub id: Option<String>,
    pub session_id: String,
    /// The agent whose rules apply; the session's when unset.
    pub agent: Option<String>,
    /// The tool call that asks (`{type: "tool", messageID, id}`).
    pub source: Option<Source>,
    /// `Permission.Unattended`: rules that settle a request which would otherwise ask, for work nobody is present to
    /// answer (a schedule's check); a request they do not allow is denied. Configured denies still win.
    pub unattended: Option<Vec<Json>>,
}

#[derive(Debug, Clone)]
pub struct Source {
    pub message_id: String,
    pub id: String,
}

/// Why a request was refused.
#[derive(Debug, Clone)]
pub enum Error {
    /// `Permission.BlockedError`: the rules deny it (`rules` are the ruleset's rules for the action).
    Blocked { rules: Vec<Json>, permission: String, resources: Vec<String>, reason: Option<String> },
    /// `Permission.CorrectedError`: rejected with feedback.
    Corrected(String),
    /// `Permission.DeclinedError`: rejected (or the location shut down).
    Declined,
    /// `Session.NotFoundError`.
    SessionNotFound(String),
}

impl Error {
    pub fn message(&self) -> String {
        match self {
            Error::Blocked { permission, reason, .. } => {
                reason.clone().unwrap_or_else(|| format!("Permission denied: {permission}"))
            }
            Error::Corrected(feedback) => feedback.clone(),
            Error::Declined => String::new(),
            Error::SessionNotFound(id) => format!("Session not found: {id}"),
        }
    }
}

/// `Permission.Reply`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    Once,
    Always,
    Reject,
}

impl Reply {
    pub fn parse(text: &str) -> Option<Reply> {
        match text {
            "once" => Some(Reply::Once),
            "always" => Some(Reply::Always),
            "reject" => Some(Reply::Reject),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Reply::Once => "once",
            Reply::Always => "always",
            Reply::Reject => "reject",
        }
    }
}

/// `AskResult`.
#[derive(Debug, Clone)]
pub struct AskResult {
    pub id: String,
    pub effect: &'static str,
}

/// `Permission.NotFoundError` of a reply.
#[derive(Debug, Clone)]
pub struct NotFound {
    pub request_id: String,
}

/// Why [`Service::ask`] failed.
#[derive(Debug, Clone)]
pub enum AskError {
    /// `Session.NotFoundError`.
    SessionNotFound(String),
    /// A pending request already has the given ID (a defect in TS: `Duplicate pending permission ID: <id>`).
    Duplicate(String),
}

type Settle = Result<(), Error>;

struct Pending {
    /// The `Permission.Request` as published.
    request: Json,
    session_id: String,
    action: String,
    resources: Vec<String>,
    save: Option<Vec<String>>,
    agent: Option<String>,
    settle: Option<oneshot::Sender<Settle>>,
}

impl Pending {
    fn id(&self) -> &str {
        json::get_str(&self.request, "id").unwrap_or_default()
    }

    fn settle(&mut self, result: Settle) {
        if let Some(sender) = self.settle.take() {
            let _ = sender.send(result);
        }
    }
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Service").field("pending", &self.list()).finish()
    }
}

/// A location's permission service (`Permission.Service`).
#[derive(Default)]
pub struct Service {
    /// Pending requests in the order they were asked.
    pending: Mutex<Vec<Pending>>,
    closed: AtomicBool,
}

struct Evaluation {
    effect: &'static str,
    message: Option<String>,
    rules: Vec<Json>,
}

fn rule_matches(rule: &Json, action: &str, resource: &str) -> bool {
    json::get_str(rule, "action").is_some_and(|pattern| wildcard_match(action, pattern))
        && json::get_str(rule, "resource").is_some_and(|pattern| wildcard_match(resource, pattern))
}

/// `Permission.evaluate`'s effect: the last rule matching the action and resource, `ask` without one.
pub fn evaluate<'a>(action: &str, resource: &str, rules: &'a [Json]) -> &'a str {
    rules
        .iter()
        .rev()
        .find(|rule| rule_matches(rule, action, resource))
        .and_then(|rule| json::get_str(rule, "effect"))
        .unwrap_or("ask")
}

fn rule(action: &str, resource: &str, effect: &str) -> Json {
    let mut o = Obj::new();
    o.str("action", action).str("resource", resource).str("effect", effect);
    o.done()
}

/// The request's ruleset (`configured`): the agent's (deny-all when unknown), then the session's.
/// `Session.NotFoundError` of an evaluation: the requester's session does not exist.
struct MissingSession(String);

impl From<MissingSession> for Error {
    fn from(MissingSession(id): MissingSession) -> Self {
        Error::SessionNotFound(id)
    }
}

fn configured(app: &App, location: &Location, requester: &Requester) -> Result<Vec<Json>, MissingSession> {
    let stored = crate::session::get(&app.db.lock().unwrap(), &requester.session_id)
        .map_err(fatal)
        .ok()
        .flatten()
        .ok_or_else(|| MissingSession(requester.session_id.clone()))?;
    let agent = requester.agent.clone().or_else(|| json::get_str(&stored.info, "agent").map(str::to_owned));
    let inventory = location.inventory();
    // `Agent.resolve`: the named agent, else the selected default.
    let resolved = match agent.as_deref() {
        Some(id) => inventory.agent(id),
        None => inventory.default_agent(),
    };
    let mut rules = match resolved {
        Some(agent) => match json::of(&agent.permissions) {
            Json::Array(rules) => rules,
            _ => vec![],
        },
        None => vec![rule("*", "*", "deny")],
    };
    if let Some(Json::Array(session)) = json::get(&stored.info, "permissions") {
        rules.extend(session.iter().cloned());
    }
    Ok(rules)
}

/// The project's saved approvals as allow rules.
fn saved_rules(app: &App, project_id: &str) -> Vec<Json> {
    let db = app.db.lock().unwrap();
    let mut statement = db.prepare_cached("SELECT action, resource FROM permission WHERE project_id = ?1").expect("permission table");
    statement
        .query_map([project_id], |row| Ok(rule(&row.get::<_, String>(0)?, &row.get::<_, String>(1)?, "allow")))
        .and_then(|rows| rows.collect())
        .unwrap_or_else(|error| {
            let _ = fatal(error);
            vec![]
        })
}

/// `PermissionSaved.add`: one approval per resource, keeping existing ones.
pub fn save(db: &rusqlite::Connection, project_id: &str, action: &str, resources: &[String]) -> rusqlite::Result<()> {
    let now = crate::id::now();
    for resource in resources {
        db.execute(
            "INSERT INTO permission (id, project_id, action, resource, time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?5, ?5) ON CONFLICT DO NOTHING",
            rusqlite::params![format!("psv_{}", crate::id::ascending()), project_id, action, resource, now],
        )?;
    }
    Ok(())
}

/// The managed policies' verdict (config/plugin/policy.ts's `permission.evaluate` hook): the first resource whose
/// last matching `tool.use` statement denies `<action>:<resource>`.
fn policy_denial(location: &Location, action: &str, resources: &[String]) -> Option<Option<String>> {
    let inventory = location.inventory();
    let statements = crate::inventory::providers::statements(&inventory.entries);
    resources.iter().find_map(|resource| {
        let subject = format!("{action}:{resource}");
        let last = statements.iter().rev().find(|s| s.action == "tool.use" && wildcard_match(&subject, &s.resource))?;
        (last.effect == "deny").then(|| last.message.clone())
    })
}

impl Service {
    fn evaluate_input(&self, app: &App, location: &Location, requester: &Requester, ask: &Ask) -> Result<Evaluation, MissingSession> {
        let rules = configured(app, location, requester)?;
        if ask.resources.iter().any(|resource| evaluate(&ask.action, resource, &rules) == "deny") {
            return Ok(Evaluation { effect: "deny", message: None, rules });
        }
        let mut all = rules;
        all.extend(saved_rules(app, &location.project.id));
        let asks = ask.resources.iter().any(|resource| evaluate(&ask.action, resource, &all) == "ask");
        let mut effect = if asks { "ask" } else { "allow" };
        let mut message = None;
        if let Some(reason) = policy_denial(location, &ask.action, &ask.resources) {
            effect = "deny";
            message = reason;
        }
        if let (true, Some(unattended)) = (effect == "ask", &requester.unattended) {
            let settled: Vec<Json> = all.iter().chain(unattended).cloned().collect();
            if ask.resources.iter().all(|resource| evaluate(&ask.action, resource, &settled) == "allow") {
                return Ok(Evaluation { effect: "allow", message: None, rules: all });
            }
            let message = format!("Permission {} needs approval, and nobody is present to give it", ask.action);
            return Ok(Evaluation { effect: "deny", message: Some(message), rules: all });
        }
        Ok(Evaluation { effect, message, rules: all })
    }

    fn request(requester: &Requester, ask: &Ask, message: Option<String>) -> Json {
        let mut o = Obj::new();
        o.str("id", &requester.id.clone().unwrap_or_else(|| format!("per_{}", crate::id::ascending())));
        o.str("sessionID", &requester.session_id).str("action", &ask.action);
        o.set("resources", Json::Array(ask.resources.iter().map(|r| Json::String(r.clone())).collect()));
        o.opt("save", ask.save.as_ref().map(|save| Json::Array(save.iter().map(|r| Json::String(r.clone())).collect())));
        o.opt("metadata", ask.metadata.clone());
        o.opt(
            "source",
            requester.source.as_ref().map(|source| {
                let mut s = Obj::new();
                s.str("type", "tool").str("messageID", &source.message_id).str("id", &source.id);
                s.done()
            }),
        );
        o.opt_str("message", message.as_deref());
        o.done()
    }

    /// Records a pending request and publishes it; the receiver settles with its reply. A request whose ID is
    /// pending already is refused (with that ID).
    fn create(
        &self,
        bus: &Bus,
        location: &LocationRef,
        request: Json,
        requester: &Requester,
        ask: &Ask,
    ) -> Result<oneshot::Receiver<Settle>, String> {
        let (sender, receiver) = oneshot::channel();
        if self.closed.load(Ordering::SeqCst) {
            let _ = sender.send(Err(Error::Declined));
            return Ok(receiver);
        }
        let id = json::get_str(&request, "id").unwrap_or_default().to_owned();
        let mut pending = self.pending.lock().unwrap();
        if pending.iter().any(|p| p.id() == id) {
            return Err(id);
        }
        pending.push(Pending {
            request: request.clone(),
            session_id: requester.session_id.clone(),
            action: ask.action.clone(),
            resources: ask.resources.clone(),
            save: ask.save.clone(),
            agent: requester.agent.clone(),
            settle: Some(sender),
        });
        drop(pending);
        bus.emit("permission.asked", request, PublishOptions { id: None, location: Some(location.clone()) });
        Ok(receiver)
    }

    /// `Permission.ask`: evaluates the request and, when it needs an answer, publishes it without waiting.
    pub fn ask(&self, app: &App, location: &Location, requester: &Requester, ask: &Ask) -> Result<AskResult, AskError> {
        if self.closed.load(Ordering::SeqCst) {
            let id = requester.id.clone().unwrap_or_else(|| format!("per_{}", crate::id::ascending()));
            return Ok(AskResult { id, effect: "deny" });
        }
        let result =
            self.evaluate_input(app, location, requester, ask).map_err(|MissingSession(id)| AskError::SessionNotFound(id))?;
        let request = Self::request(requester, ask, result.message);
        let id = json::get_str(&request, "id").unwrap_or_default().to_owned();
        if result.effect == "ask" {
            // Nobody awaits the answer: dropping the receiver leaves the request pending until it is replied to.
            drop(self.create(&app.bus, &location.reference(), request, requester, ask).map_err(AskError::Duplicate)?);
        }
        Ok(AskResult { id, effect: result.effect })
    }

    /// `Permission.assert`: allowed, or asks and waits for the reply. A wait that is dropped (the call interrupted)
    /// forgets the request.
    pub async fn assert(&self, app: &App, location: &Location, requester: &Requester, ask: &Ask) -> Result<(), Error> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(Error::Declined);
        }
        let result = self.evaluate_input(app, location, requester, ask)?;
        match result.effect {
            "allow" => Ok(()),
            "deny" => Err(Error::Blocked {
                rules: result.rules.into_iter().filter(|rule| matches_action(rule, &ask.action)).collect(),
                permission: ask.action.clone(),
                resources: ask.resources.clone(),
                reason: result.message,
            }),
            _ => {
                let request = Self::request(requester, ask, result.message);
                let id = json::get_str(&request, "id").unwrap_or_default().to_owned();
                // A tool's requests get fresh IDs, which are never pending already.
                let receiver = self.create(&app.bus, &location.reference(), request, requester, ask).map_err(|id| {
                    eprintln!("Duplicate pending permission ID: {id}");
                    Error::Declined
                })?;
                let _forget = Forget { service: self, id };
                receiver.await.unwrap_or(Err(Error::Declined))
            }
        }
    }

    /// `Permission.assertNotDenied`: fails only when the rules deny the request; never asks.
    pub fn assert_not_denied(&self, app: &App, location: &Location, requester: &Requester, ask: &Ask) -> Result<(), Error> {
        let result = self.evaluate_input(app, location, requester, ask)?;
        if result.effect != "deny" {
            return Ok(());
        }
        Err(Error::Blocked {
            rules: result.rules.into_iter().filter(|rule| matches_action(rule, &ask.action)).collect(),
            permission: ask.action.clone(),
            resources: ask.resources.clone(),
            reason: result.message,
        })
    }

    /// `Permission.allows`: whether the rules already allow the request; never asks.
    pub fn allows(&self, app: &App, location: &Location, requester: &Requester, ask: &Ask) -> Result<bool, Error> {
        if self.closed.load(Ordering::SeqCst) {
            return Ok(false);
        }
        Ok(self.evaluate_input(app, location, requester, ask)?.effect == "allow")
    }

    /// `Permission.reply`. A rejection settles every other pending request of the session the same way; an "always"
    /// reply saves the request's `save` resources and settles the other requests the saved approvals now allow.
    pub fn reply(
        &self,
        app: &App,
        location: &Location,
        request_id: &str,
        reply: Reply,
        message: Option<&str>,
    ) -> Result<(), NotFound> {
        let reference = location.reference();
        let options = || PublishOptions { id: None, location: Some(reference.clone()) };
        let replied = |session_id: &str, id: &str, reply: Reply| {
            let mut o = Obj::new();
            o.str("sessionID", session_id).str("requestID", id).str("reply", reply.as_str());
            app.bus.emit("permission.replied", o.done(), options());
        };
        let rejection = || match message {
            Some(feedback) if !feedback.is_empty() => Error::Corrected(feedback.to_owned()),
            _ => Error::Declined,
        };
        let mut existing = {
            let mut pending = self.pending.lock().unwrap();
            let Some(index) = pending.iter().position(|p| p.id() == request_id) else {
                return Err(NotFound { request_id: request_id.to_owned() });
            };
            pending.remove(index)
        };
        replied(&existing.session_id, existing.id(), reply);
        if reply == Reply::Reject {
            existing.settle(Err(rejection()));
            let others: Vec<Pending> = {
                let mut pending = self.pending.lock().unwrap();
                let (same, rest): (Vec<Pending>, Vec<Pending>) =
                    std::mem::take(&mut *pending).into_iter().partition(|p| p.session_id == existing.session_id);
                *pending = rest;
                same
            };
            for mut other in others {
                replied(&other.session_id, other.id(), Reply::Reject);
                other.settle(Err(rejection()));
            }
            return Ok(());
        }
        let saves = reply == Reply::Always && existing.save.as_ref().is_some_and(|save| !save.is_empty());
        if saves {
            let db = app.db.lock().unwrap();
            if let Err(error) = save(&db, &location.project.id, &existing.action, existing.save.as_deref().unwrap_or_default()) {
                let _ = fatal(error);
            }
        }
        existing.settle(Ok(()));
        if !saves {
            return Ok(());
        }
        let ids: Vec<String> = self.pending.lock().unwrap().iter().map(|p| p.id().to_owned()).collect();
        for id in ids {
            let (requester, ask) = {
                let pending = self.pending.lock().unwrap();
                let Some(item) = pending.iter().find(|p| p.id() == id) else { continue };
                (
                    Requester {
                        id: Some(id.clone()),
                        session_id: item.session_id.clone(),
                        agent: item.agent.clone(),
                        source: None,
                        unattended: None,
                    },
                    Ask { action: item.action.clone(), resources: item.resources.clone(), save: None, metadata: None },
                )
            };
            let Ok(result) = self.evaluate_input(app, location, &requester, &ask) else { continue };
            if result.effect != "allow" {
                continue;
            }
            let item = {
                let mut pending = self.pending.lock().unwrap();
                pending.iter().position(|p| p.id() == id).map(|index| pending.remove(index))
            };
            if let Some(mut item) = item {
                replied(&item.session_id, &id, Reply::Always);
                item.settle(Ok(()));
            }
        }
        Ok(())
    }

    /// `Permission.list`: the pending requests, oldest first.
    pub fn list(&self) -> Vec<Json> {
        self.pending.lock().unwrap().iter().map(|p| p.request.clone()).collect()
    }

    /// `Permission.get`.
    pub fn get(&self, id: &str) -> Option<Json> {
        self.pending.lock().unwrap().iter().find(|p| p.id() == id).map(|p| p.request.clone())
    }

    /// `Permission.forSession`.
    pub fn for_session(&self, session_id: &str) -> Vec<Json> {
        self.pending.lock().unwrap().iter().filter(|p| p.session_id == session_id).map(|p| p.request.clone()).collect()
    }

    /// Whether the location shut down (`LocationLifecycle.isClosed`: the permissions close first).
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// The location shuts down: every pending request is rejected (`close`).
    pub fn close(&self, bus: &Bus, location: &LocationRef) {
        self.closed.store(true, Ordering::SeqCst);
        let pending = std::mem::take(&mut *self.pending.lock().unwrap());
        for mut item in pending {
            let mut o = Obj::new();
            o.str("sessionID", &item.session_id).str("requestID", item.id()).str("reply", "reject");
            bus.emit("permission.replied", o.done(), PublishOptions { id: None, location: Some(location.clone()) });
            item.settle(Err(Error::Declined));
        }
    }
}

fn matches_action(rule: &Json, action: &str) -> bool {
    json::get_str(rule, "action").is_some_and(|pattern| wildcard_match(action, pattern))
}

/// Removes a request whose wait ended (answered, or the waiting call interrupted).
struct Forget<'a> {
    service: &'a Service,
    id: String,
}

impl Drop for Forget<'_> {
    fn drop(&mut self) {
        self.service.pending.lock().unwrap().retain(|p| p.id() != self.id);
    }
}

/// `PermissionNotFoundError` (`missingRequest` in handlers/permission.ts).
pub fn not_found(request_id: &str) -> HttpFailure {
    let mut o = Obj::new();
    o.str("_tag", "PermissionNotFoundError").str("requestID", request_id);
    o.str("message", &format!("Permission request not found: {request_id}"));
    HttpFailure::new(404, o.done())
}
