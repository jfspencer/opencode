//! Session execution (packages/core/src/session/execution.ts, session/run-coordinator.ts): one busy period per
//! session, owned by one task that drains the session's work until it would stay idle. Work recorded meanwhile rings
//! the doorbell (`wake`), and the execution drains again instead of ending; a doorbell rung after the loop decided to
//! end starts a successor execution. Interruption is cooperative: the execution's [`Cancel`] stops the step at its
//! next await, and the step still records what it must (failed tools, the interrupted step) before the execution
//! settles.
//!
//! The execution's lifecycle events are `session.execution.started` (which claims the session: `time_suspended`)
//! and one terminal event per busy period: `succeeded`, `failed` (with the session error) or `interrupted` (with
//! its reason); their projections set the session's busy and idle times. A claim that survives the process is
//! picked up at the next boot ([`resume_suspended`]). A deliberate interruption cancels the session's own job (a
//! subagent's run); [`Executions::run`] joins or starts a busy period and resolves with its [`Exit`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{Notify, watch};

use crate::app::App;
use crate::json::Obj;
use crate::session::inbox::Promotable;
use crate::session::step::Stop;

/// A cooperative cancellation signal.
#[derive(Clone, Default)]
pub struct Cancel(Arc<(AtomicBool, Notify)>);

impl Cancel {
    pub fn cancel(&self) {
        self.0.0.store(true, Ordering::SeqCst);
        self.0.1.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.0.load(Ordering::SeqCst)
    }

    /// Resolves once cancelled.
    pub async fn cancelled(&self) {
        loop {
            let notified = self.0.1.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

/// How a busy period ended (the exit its joiners get, `SessionRunCoordinator.run`).
#[derive(Debug, Clone)]
pub enum Exit {
    Succeeded,
    Failed(opencode_protocol::Json),
    Interrupted,
}

struct Execution {
    done: watch::Sender<Option<Exit>>,
    scope: Promotable,
    pending: Option<Promotable>,
    stopping: bool,
    reason: Option<&'static str>,
    running: bool,
    cancel: Cancel,
}

#[derive(Default)]
pub struct Executions {
    map: Mutex<HashMap<String, Execution>>,
}

impl Executions {
    /// Sessions with an execution owned by this process (`SessionExecution.active`).
    pub fn active(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.map.lock().unwrap().keys().cloned().collect();
        ids.sort();
        ids
    }

    /// `SessionExecution.isActive` (revert staging refuses a busy session).
    pub fn is_active(&self, session: &str) -> bool {
        self.map.lock().unwrap().contains_key(session)
    }

    /// Rings the doorbell: an idle session starts an execution, an active one drains again before settling.
    pub fn wake(&self, app: &App, session: &str, scope: Promotable) {
        let mut map = self.map.lock().unwrap();
        if let Some(execution) = map.get_mut(session) {
            // Coalesced wakes keep the widest scope: `input` subsumes `steer`.
            execution.pending = Some(if execution.pending == Some(Promotable::Input) { Promotable::Input } else { scope });
            return;
        }
        start(app, &mut map, session, false, scope);
    }

    /// `SessionExecution.resume`: starts an execution that drains even without new input, unless one is active.
    pub fn resume(&self, app: &App, session: &str) {
        let mut map = self.map.lock().unwrap();
        if !map.contains_key(session) {
            start(app, &mut map, session, true, Promotable::Input);
        }
    }

    /// Stops the active execution; `false` when idle. Resolves before cleanup settles.
    pub fn interrupt(&self, session: &str, reason: &'static str) -> bool {
        let mut map = self.map.lock().unwrap();
        let Some(execution) = map.get_mut(session) else { return false };
        if execution.stopping {
            return false;
        }
        if !execution.running {
            // Settlement window: the outcome is decided; the interrupt only claims the recorded wakes.
            execution.pending = None;
            return false;
        }
        execution.stopping = true;
        execution.pending = None;
        execution.reason = Some(reason);
        execution.cancel.cancel();
        true
    }

    /// Resolves once no execution is active for the session.
    pub async fn await_idle(&self, session: &str) {
        loop {
            let receiver = match self.map.lock().unwrap().get(session) {
                Some(execution) => execution.done.subscribe(),
                None => return,
            };
            let mut receiver = receiver;
            let _ = receiver.wait_for(Option::is_some).await;
        }
    }

    /// `SessionRunCoordinator.run`: joins the active execution, or starts one that drains even without new input,
    /// and resolves with its exit. The execution starts (or is joined) at the call, not when the future is polled;
    /// a stopping execution is waited out, then a fresh one runs.
    pub fn run(&self, app: &App, session: &str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Exit> + Send>> {
        let mut map = self.map.lock().unwrap();
        let stopping = map.get(session).map(|execution| execution.stopping);
        if stopping.is_none() {
            start(app, &mut map, session, true, Promotable::Input);
        }
        let mut receiver = map.get(session).expect("started").done.subscribe();
        drop(map);
        let app = app.arc();
        let session = session.to_owned();
        Box::pin(async move {
            let exit = receiver.wait_for(Option::is_some).await.ok().and_then(|exit| exit.clone()).unwrap_or(Exit::Interrupted);
            if stopping != Some(true) {
                return exit;
            }
            app.sessions.executions.run(&app, &session).await
        })
    }
}

/// Restart continuity: see [`crate::session::restart`].
pub fn resume_suspended(app: &App) {
    crate::session::restart::resume_suspended(app);
}

fn start(app: &App, map: &mut HashMap<String, Execution>, session: &str, force: bool, scope: Promotable) {
    let (done, _) = watch::channel(None);
    let cancel = Cancel::default();
    map.insert(
        session.to_owned(),
        Execution { done, scope, pending: None, stopping: false, reason: None, running: true, cancel: cancel.clone() },
    );
    let app = app.arc();
    let session = session.to_owned();
    tokio::spawn(async move { run(app, session, force, scope, cancel).await });
}

async fn run(app: Arc<App>, session: String, force: bool, scope: Promotable, cancel: Cancel) {
    // Drains start one tick after the wake.
    tokio::task::yield_now().await;
    let mut data = Obj::new();
    data.str("sessionID", &session);
    if let Err(error) = crate::session::publish_event(&app, "session.execution.started", data.done(), None, None) {
        eprintln!("failed to publish the execution start: {error}");
    }
    let executions = &app.sessions.executions;
    let mut force = force;
    let mut scope = scope;
    let mut moved: Option<(Option<i64>, bool)> = None;
    let result = loop {
        let (continuation, stepped) = moved.take().unwrap_or((None, false));
        let input = crate::session::runner::Drain { force, promotable: scope, continuation, stepped };
        let result = crate::session::runner::drain(&app, &session, input, &cancel).await;
        force = false;
        match result {
            Err(stop) => break Err(stop),
            // A move continues the turn in the session's new location.
            Ok(crate::session::runner::Drained::Moved { continuation, stepped }) => {
                moved = Some((continuation, stepped));
                continue;
            }
            Ok(crate::session::runner::Drained::Complete) => {}
        }
        // The doorbell: drain again when work arrived during the drain.
        let next = {
            let mut map = executions.map.lock().unwrap();
            map.get_mut(&session).and_then(|execution| match execution.pending.take() {
                Some(next) if !execution.stopping => {
                    execution.scope = next;
                    Some(next)
                }
                _ => None,
            })
        };
        let Some(next) = next else { break Ok(()) };
        scope = next;
        tokio::task::yield_now().await;
    };
    let reason = {
        let mut map = executions.map.lock().unwrap();
        map.get_mut(&session).map(|execution| {
            execution.running = false;
            execution.reason
        })
    }
    .flatten();
    let mut data = Obj::new();
    data.str("sessionID", &session);
    let (published, exit) = match result {
        Ok(()) => (crate::session::publish_event(&app, "session.execution.succeeded", data.done(), None, None), Exit::Succeeded),
        Err(Stop::Interrupted) => {
            // An execution that stopped itself (a declined tool call) was stopped by the user.
            let reason = reason.unwrap_or(if cancel.is_cancelled() { "shutdown" } else { "user" });
            // A deliberate stop cancels the session's own job (a subagent's); shutdown keeps it for recovery.
            if reason != "shutdown" {
                app.sessions.jobs.cancel(&app, &session);
            }
            data.str("reason", reason);
            (crate::session::publish_event(&app, "session.execution.interrupted", data.done(), None, None), Exit::Interrupted)
        }
        Err(Stop::Failed(error)) => {
            data.set("error", error.clone());
            (crate::session::publish_event(&app, "session.execution.failed", data.done(), None, None), Exit::Failed(error))
        }
    };
    if let Err(error) = published {
        eprintln!("failed to publish the execution outcome: {error}");
    }
    let mut map = executions.map.lock().unwrap();
    let Some(execution) = map.remove(&session) else { return };
    execution.done.send_replace(Some(exit));
    if let Some(next) = execution.pending {
        start(&app, &mut map, &session, false, next);
    }
}
