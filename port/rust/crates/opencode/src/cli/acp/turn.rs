//! One `session/prompt` (packages/cli/src/acp/turn.ts): the submission, the event stream folded into updates until
//! the turn ends, the asks (permission requests and forms) that reach the client one at a time, cancellation
//! (with a drain before in-flight tools are reported cancelled), children followed after the turn, and the response.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opencode_protocol::Json;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use super::prompt::Prepared;
use super::translate::{self, Context, Output, State, Terminal};
use super::{Agent, Attached, Catalog, RpcError, elicitation, permission};
use crate::json::{self, Obj, get, get_num, get_str};

/// Core acknowledges an interrupt before its cleanup settles, and its shell tool waits 3s before SIGKILL.
const CANCEL_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

type Task = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The events a turn reads and the queue its asks wait in; a background follower takes it over from the turn.
pub struct Subscription {
    events: tokio::sync::Mutex<mpsc::UnboundedReceiver<Result<Json, RpcError>>>,
    feed: JoinHandle<()>,
    asks: mpsc::UnboundedSender<Task>,
    worker: Mutex<Option<JoinHandle<()>>>,
    settled: Mutex<HashMap<String, oneshot::Sender<()>>>,
    /// Closing ends the ask in progress with its fallback (still answered) and starts no other.
    closed: watch::Sender<bool>,
}

impl Subscription {
    fn open(agent: &Arc<Agent>) -> Arc<Subscription> {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let client = agent.client.clone();
        let feed = tokio::spawn(async move {
            let mut events = match client.events().await {
                Ok(events) => events,
                Err(_) => {
                    let _ = events_tx.send(Err(RpcError::unavailable()));
                    return;
                }
            };
            loop {
                match events.next().await {
                    Some(Ok(event)) => {
                        if events_tx.send(Ok(event)).is_err() {
                            return;
                        }
                    }
                    _ => {
                        let _ = events_tx.send(Err(RpcError::unavailable()));
                        return;
                    }
                }
            }
        });
        let (asks, mut queue) = mpsc::unbounded_channel::<Task>();
        let closed = watch::channel(false).0;
        let mut closing = closed.subscribe();
        let worker = tokio::spawn(async move {
            loop {
                let task = tokio::select! {
                    biased;
                    _ = closing.wait_for(|c| *c) => return,
                    task = queue.recv() => task,
                };
                let Some(task) = task else { return };
                task.await;
            }
        });
        Arc::new(Subscription {
            events: tokio::sync::Mutex::new(events_rx),
            feed,
            asks,
            worker: Mutex::new(Some(worker)),
            settled: Mutex::new(HashMap::new()),
            closed,
        })
    }

    async fn take(&self) -> Result<Json, RpcError> {
        self.events.lock().await.recv().await.unwrap_or_else(|| Err(RpcError::unavailable()))
    }

    fn close(&self) {
        let _ = self.closed.send(true);
        self.feed.abort();
    }

    /// Closes, and waits until the ask in progress has been answered.
    async fn close_and_drain(&self) {
        self.close();
        let worker = self.worker.lock().unwrap().take();
        if let Some(worker) = worker {
            let _ = worker.await;
        }
    }

    /// `asksSettled`: waits for every ask queued so far.
    async fn asks_settled(&self) {
        let (tx, rx) = oneshot::channel();
        let marker: Task = Box::pin(async move {
            let _ = tx.send(());
        });
        if self.asks.send(marker).is_ok() {
            let _ = rx.await;
        }
    }
}

/// Closes a subscription when its last user goes, however it goes.
struct Closing(Arc<Subscription>);

impl Drop for Closing {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[derive(Clone)]
struct Turn {
    ctx: Context,
    subscription: Arc<Subscription>,
    cancelled: watch::Sender<bool>,
    background: bool,
}

enum Exit {
    Done(Terminal),
    Failed(RpcError),
    Interrupted,
}

/// `ACPTurn.prompt`: the turn runs to its response; a cancel interrupts it, and still answers.
pub async fn run(agent: &Arc<Agent>, params: &Json, mut cancel: watch::Receiver<bool>) -> super::Reply {
    let session_id = get_str(params, "sessionId").unwrap_or("").to_owned();
    let attached = agent.attached(&session_id)?;
    let catalog = agent.catalog(&attached.cwd).await?;
    let prepared = super::prompt::prepare(&catalog.commands, get(params, "prompt").and_then(json::array).unwrap_or(&[])).await?;
    let capabilities = agent.capabilities();
    let ctx = Context {
        session_id: session_id.clone(),
        cwd: attached.cwd.clone(),
        start: prepared.id.clone(),
        child_updates: capabilities.child_updates,
        compaction: capabilities.compaction,
    };
    let state = Arc::new(Mutex::new(State::default()));
    let turn = Turn { ctx, subscription: Subscription::open(agent), cancelled: watch::channel(false).0, background: false };
    let exit = execute(agent, &attached, &prepared, &turn, &state, &mut cancel).await;
    handoff(agent, &attached, &turn, &state, &exit).await;
    settle(agent, &attached, &catalog, &state, exit, &cancel).await
}

async fn execute(agent: &Arc<Agent>, attached: &Arc<Attached>, prepared: &Prepared, turn: &Turn, state: &Arc<Mutex<State>>, cancel: &mut watch::Receiver<bool>) -> Exit {
    // The feed opens with `server.connected`, so every event the submission causes comes after it.
    let connected = tokio::select! {
        biased;
        _ = cancel.wait_for(|c| *c) => None,
        connected = turn.subscription.take() => Some(connected),
    };
    match connected {
        None => {
            wind_down(agent, turn, state, &mut None).await;
            return Exit::Interrupted;
        }
        Some(Err(error)) => return Exit::Failed(error),
        Some(Ok(event)) if get_str(&event, "type") != Some("server.connected") => {
            return Exit::Failed(RpcError::service("Internal service failure", None, Some("Error")));
        }
        Some(Ok(_)) => {}
    }
    let mut consumer = Some(tokio::spawn(consume(agent.clone(), turn.clone(), state.clone())));
    let finished = tokio::select! {
        biased;
        _ = cancel.wait_for(|c| *c) => None,
        result = async {
            submit(agent, &attached.id, prepared).await?;
            if prepared.command {
                return Ok(Terminal::Succeeded);
            }
            let result = consumer.as_mut().expect("the consumer runs").await;
            consumer = None;
            result.unwrap_or_else(|_| Err(RpcError::unavailable()))
        } => Some(result),
    };
    let exit = match finished {
        None => {
            wind_down(agent, turn, state, &mut consumer).await;
            Exit::Interrupted
        }
        Some(Ok(terminal)) => Exit::Done(terminal),
        Some(Err(error)) => Exit::Failed(error),
    };
    if let Some(consumer) = consumer {
        consumer.abort();
        let _ = consumer.await;
    }
    exit
}

/// `windDown`: tells the server, waits for the turn to drain, and reports the tools it left running as cancelled.
async fn wind_down(agent: &Arc<Agent>, turn: &Turn, state: &Arc<Mutex<State>>, consumer: &mut Option<JoinHandle<Result<Terminal, RpcError>>>) {
    let _ = turn.cancelled.send(true);
    // The cancel answers the open asks with their fallbacks (a rejection, a cancelled form); in TS those answers
    // reach the server before its interrupt has taken effect, so the asking tool fails as declined. Answering them
    // first keeps that outcome; none of them asks the client any more.
    turn.subscription.asks_settled().await;
    let _ = agent.client.call("session.interrupt", &[("sessionID", &turn.ctx.session_id)], None, None).await;
    if !state.lock().unwrap().started {
        return;
    }
    let Some(handle) = consumer.as_mut() else { return };
    if let Ok(result) = tokio::time::timeout(CANCEL_DRAIN_TIMEOUT, handle).await {
        *consumer = None;
        if matches!(result, Ok(Ok(_))) {
            return;
        }
    }
    if let Some(handle) = consumer.take() {
        handle.abort();
        let _ = handle.await;
    }
    let outputs = translate::abandon(&mut state.lock().unwrap(), &turn.ctx);
    for output in outputs {
        interpret(agent, turn, output).await;
    }
}

async fn submit(agent: &Agent, session_id: &str, prepared: &Prepared) -> Result<(), RpcError> {
    let client = &agent.client;
    if !prepared.synthetic.is_empty() {
        let mut body = Obj::new();
        body.str("text", &prepared.synthetic.join("\n\n"))
            .str("description", "ACP embedded context")
            .str("delivery", "steer")
            .set("resume", Json::Bool(false));
        client.call("session.synthetic", &[("sessionID", session_id)], None, Some(body.done())).await.map_err(super::classify)?;
    }
    if prepared.compaction {
        let mut body = Obj::new();
        body.str("id", &prepared.id);
        client.call("session.compact", &[("sessionID", session_id)], None, Some(body.done())).await.map_err(super::classify)?;
        return Ok(());
    }
    if prepared.command {
        let (name, args) = prepared.slash.clone().unwrap_or_default();
        let mut body = Obj::new();
        body.str("name", &name).str("text", &args).set("files", Json::Array(prepared.files.clone())).str("delivery", "steer");
        client.call("session.command", &[("sessionID", session_id)], None, Some(body.done())).await.map_err(super::classify)?;
        return Ok(());
    }
    let mut body = Obj::new();
    body.str("id", &prepared.id).str("text", &prepared.text).set("files", Json::Array(prepared.files.clone())).str("delivery", "steer");
    client.call("session.prompt", &[("sessionID", session_id)], None, Some(body.done())).await.map_err(super::classify)?;
    Ok(())
}

/// One event: folded, and its outputs interpreted in order.
async fn advance(agent: &Arc<Agent>, turn: &Turn, state: &Arc<Mutex<State>>) -> Result<translate::Folded, RpcError> {
    let event = turn.subscription.take().await?;
    let folded = {
        let mut current = state.lock().unwrap();
        if turn.background && !translate::from_tracked_child(&current, &event) {
            translate::Folded { outputs: vec![], terminal: None }
        } else {
            translate::fold(&mut current, &event, &turn.ctx)
        }
    };
    let terminal = folded.terminal;
    for output in folded.outputs {
        interpret(agent, turn, output).await;
    }
    Ok(translate::Folded { outputs: vec![], terminal })
}

/// `consume`: the turn's events until its terminal one, then its asks settle.
async fn consume(agent: Arc<Agent>, turn: Turn, state: Arc<Mutex<State>>) -> Result<Terminal, RpcError> {
    loop {
        if let Some(terminal) = advance(&agent, &turn, &state).await?.terminal {
            turn.subscription.asks_settled().await;
            return Ok(terminal);
        }
    }
}

/// `followChildren`: after the turn, its children's updates and asks until none is running.
async fn follow_children(agent: Arc<Agent>, turn: Turn, state: Arc<Mutex<State>>) -> Result<(), RpcError> {
    loop {
        advance(&agent, &turn, &state).await?;
        if state.lock().unwrap().open_children.is_empty() {
            turn.subscription.asks_settled().await;
            return Ok(());
        }
    }
}

/// `handoff`: children outlive the turn, so their updates and asks still reach the client.
async fn handoff(agent: &Arc<Agent>, attached: &Arc<Attached>, turn: &Turn, state: &Arc<Mutex<State>>, exit: &Exit) {
    let current = state.lock().unwrap().clone();
    if matches!(exit, Exit::Failed(_)) || current.open_children.is_empty() {
        return turn.subscription.close_and_drain().await;
    }
    let closing = Closing(turn.subscription.clone());
    let background = Turn { ctx: turn.ctx.clone(), subscription: turn.subscription.clone(), cancelled: watch::channel(false).0, background: true };
    let agent_task = agent.clone();
    let follow = async move {
        let _closing = closing;
        let _ = follow_children(agent_task, background, Arc::new(Mutex::new(current))).await;
    };
    agent.fork(attached, follow);
}

async fn settle(agent: &Arc<Agent>, attached: &Arc<Attached>, catalog: &Catalog, state: &Arc<Mutex<State>>, exit: Exit, cancel: &watch::Receiver<bool>) -> super::Reply {
    let terminal = match exit {
        Exit::Failed(error) => return Err(error),
        Exit::Done(terminal) => terminal,
        Exit::Interrupted => Terminal::Interrupted,
    };
    let current = state.lock().unwrap().clone();
    if terminal != Terminal::Interrupted {
        if let Some(failure) = translate::failure(&current) {
            return Err(failure);
        }
    }
    usage_update(agent, attached, catalog, &current).await;
    let terminal = if *cancel.borrow() { Terminal::Interrupted } else { terminal };
    Ok(translate::response(&current, &attached.id, terminal))
}

/// `sendUsageUpdate`: the last step's tokens against the model's context.
async fn usage_update(agent: &Agent, attached: &Attached, catalog: &Catalog, state: &State) {
    let used = state.usage.as_ref().map(|(_, last)| translate::total(last)).unwrap_or(0.0);
    if used == 0.0 {
        return;
    }
    let catalog = agent.catalog(&attached.cwd).await.unwrap_or_else(|_| catalog.clone());
    let selection = attached.selection.lock().unwrap().clone();
    let Some(model) = super::find_model(&catalog.models, &catalog.current(&selection)) else { return };
    let Some(size) = get(model, "limit").and_then(|l| get_num(l, "context")).filter(|c| *c > 0.0) else { return };
    let Ok(info) = agent.client.call("session.get", &[("sessionID", &attached.id)], None, None).await else { return };
    let mut cost = Obj::new();
    cost.set("amount", get(&info, "data").and_then(|d| get(d, "cost")).cloned().unwrap_or(Json::Number(0.0))).str("currency", "USD");
    let mut update = Obj::new();
    update.str("sessionUpdate", "usage_update").set("used", Json::Number(used)).set("size", Json::Number(size)).set("cost", cost.done());
    agent.conn.update(&attached.id, update.done());
}

async fn interpret(agent: &Arc<Agent>, turn: &Turn, output: Output) {
    match output {
        Output::SessionUpdate { update, diff } => {
            if turn.background {
                return;
            }
            let update = permission::with_completed_diffs(update, diff.as_ref(), &turn.ctx.cwd).await;
            agent.conn.update(&turn.ctx.session_id, update);
        }
        Output::ChildUpdate { mut update, diff } => {
            if get_str(&update, "type") == Some("update") {
                let inner = get(&update, "update").cloned().unwrap_or(Json::Null);
                let inner = permission::with_completed_diffs(inner, diff.as_ref(), &turn.ctx.cwd).await;
                json::set(&mut update, "update", inner);
            }
            agent.conn.notify(translate::CHILD_UPDATE_METHOD, update);
        }
        Output::PermissionAsk { event, tool, child } => {
            let ask = permission::Ask {
                session_id: get(&event, "data").and_then(|d| get_str(d, "sessionID")).unwrap_or("").to_owned(),
                event,
                client_session_id: turn.ctx.session_id.clone(),
                cwd: turn.ctx.cwd.clone(),
                tool,
                child,
            };
            let id = get(&ask.event, "data").and_then(|d| get_str(d, "id")).unwrap_or("").to_owned();
            let ask = Arc::new(ask);
            let (asking, responding) = (ask.clone(), ask);
            let (conn, client) = (agent.conn.clone(), agent.client.clone());
            queue_ask(
                turn,
                &id,
                async move {
                    let params = permission::request(&asking).await;
                    permission::decision(&conn.request("session/request_permission", params).await)
                },
                "reject",
                move |decision| async move { permission::respond(&client, &responding, decision).await },
            );
        }
        Output::FormAsk { form, child } => {
            let Some(schema) = elicitation::requested_schema(&form, agent.capabilities().form_elicitation) else {
                return elicitation::cancel_unshown(&agent.client, &form).await;
            };
            let id = get_str(&form, "id").unwrap_or("").to_owned();
            let tool_call_sent = !turn.background && (child.is_none() || !turn.ctx.child_updates);
            let ask = Arc::new(elicitation::Ask { form, schema, client_session_id: turn.ctx.session_id.clone(), child, tool_call_sent });
            let (asking, responding) = (ask.clone(), ask);
            let (conn, client) = (agent.conn.clone(), agent.client.clone());
            queue_ask(
                turn,
                &id,
                async move {
                    let response = conn.request("elicitation/create", elicitation::request(&asking)).await;
                    elicitation::answer(&asking.form, &response)
                },
                None,
                move |outcome| async move { elicitation::respond(&client, &responding.form, outcome).await },
            );
        }
        Output::AskSettled { id } => {
            if let Some(settled) = turn.subscription.settled.lock().unwrap().remove(&id) {
                let _ = settled.send(());
            }
        }
    }
}

/// `queueAsk`: asks reach the client one at a time. Each races the turn's cancel (the fallback answers), a reply
/// from elsewhere (nothing more to do) and the client; the race starts its racers in that order, so an earlier
/// cancel never sends the request, and a request that loses is withdrawn (`$/cancel_request`).
fn queue_ask<A, F, R, RF>(turn: &Turn, id: &str, ask: F, fallback: A, respond: R)
where
    A: Send + 'static,
    F: Future<Output = A> + Send + 'static,
    R: FnOnce(A) -> RF + Send + 'static,
    RF: Future<Output = ()> + Send + 'static,
{
    let (settled_tx, settled_rx) = oneshot::channel::<()>();
    turn.subscription.settled.lock().unwrap().insert(id.to_owned(), settled_tx);
    let mut cancelled = turn.cancelled.subscribe();
    let mut closed = turn.subscription.closed.subscribe();
    let task: Task = Box::pin(async move {
        let settled = async move {
            if settled_rx.await.is_err() {
                std::future::pending::<()>().await;
            }
        };
        let outcome = tokio::select! {
            biased;
            _ = cancelled.wait_for(|c| *c) => Some(fallback),
            _ = closed.wait_for(|c| *c) => Some(fallback),
            _ = settled => None,
            outcome = ask => Some(outcome),
        };
        if let Some(outcome) = outcome {
            respond(outcome).await;
        }
    });
    let _ = turn.subscription.asks.send(task);
}
