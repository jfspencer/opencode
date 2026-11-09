//! One execution's job queue (runner.ts). Program code runs only inside a job, one job at a time and each to
//! completion: promise reactions and resumed awaits queue in order, host work runs concurrently and settles its
//! promise as a later job, and the program is stuck exactly when no job is queued and no host work is running.
//!
//! TS keeps an explicit stack of generator activations and splits it at the innermost boundary that handles a
//! signal. Here an activation is a Rust future: a leaf (`await`, `yield`, a blocking host call, a time slice) records
//! its `Signal` and returns `Pending`, and the pending state propagates outward until a `Boundary` that handles the
//! signal takes the future off the stack (a parked await becomes a task resumed by a promise reaction; a yield stores
//! the generator body in its object). Unhandled host and slice signals reach the runner, which returns `Pending` to
//! its executor and polls the same task again.

use crate::interp::errors::{Diagnostic, Fail, plain_error, range_error, type_error};
use crate::interp::limits::{MAX_CALL_DEPTH, MAX_PENDING_PROMISES};
use crate::interp::object::get;
use crate::interp::value::*;
use crate::interp::{Ctx, Fut, Loc, R, boxed};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::Instant;

pub type Completion = Result<Value, Fail>;

/// What a suspended leaf records before returning `Pending`.
pub enum Signal {
    Await { value: Value, loc: Option<Loc> },
    Yield(Value),
    Host,
    Slice,
}

/// What a suspended leaf continues with.
pub enum Input {
    Value(Value),
    Error(Fail),
    Request(GenRequest),
}

impl From<Completion> for Input {
    fn from(completion: Completion) -> Input {
        match completion {
            Ok(value) => Input::Value(value),
            Err(error) => Input::Error(error),
        }
    }
}

/// A settled promise's reaction, run as a job: it finishes at once or starts a task. Runs at most once.
pub struct Reaction(RefCell<Option<Box<dyn FnOnce(&Ctx, Completion) -> Option<Start>>>>);

impl Reaction {
    pub fn new(body: impl FnOnce(&Ctx, Completion) -> Option<Start> + 'static) -> Rc<Reaction> {
        Rc::new(Reaction(RefCell::new(Some(Box::new(body)))))
    }
    fn run(&self, ctx: &Ctx, settled: Completion) -> Option<Start> {
        let body = self.0.borrow_mut().take()?;
        body(ctx, settled)
    }
}

/// The functions that settle one promise, at most once between them.
#[derive(Clone)]
pub struct Resolving {
    promise: Obj,
    done: Rc<Cell<bool>>,
    site: Option<Loc>,
}

impl Resolving {
    pub fn resolve(&self, ctx: &Ctx, resolution: Value) {
        if self.done.replace(true) {
            return;
        }
        ctx.runner.resolve_with(ctx, &self.promise, resolution, self.site);
    }
    /// Rejects with the failure as thrown; a failure without a location is located at the creating call.
    pub fn reject(&self, ctx: &Ctx, reason: Fail) {
        if self.done.replace(true) {
            return;
        }
        ctx.runner.settle(ctx, &self.promise, Err(reason.at(self.site)));
    }
    pub fn settle(&self, ctx: &Ctx, completion: Completion) {
        match completion {
            Ok(value) => self.resolve(ctx, value),
            Err(error) => self.reject(ctx, error),
        }
    }
    pub fn promise(&self) -> &Obj {
        &self.promise
    }
}

/// Where a suspended body splits off, and what the activation below receives.
pub enum Boundary {
    /// An async body (an async function, a tool call's argument crossing, the program): its completion settles a
    /// promise, and the caller receives `result` at once.
    Async { settle: Box<dyn Fn(&Ctx, Completion)>, result: Value },
    /// A generator body: a yield suspends it into its object.
    Generator(Obj),
}

impl Boundary {
    fn handles_await(&self) -> bool {
        match self {
            Boundary::Async { .. } => true,
            Boundary::Generator(generator) => generator.generator().is_some_and(|g| g.asynchronous),
        }
    }
    fn handles_yield(&self) -> bool {
        matches!(self, Boundary::Generator(_))
    }
    fn result(&self) -> Option<Value> {
        match self {
            Boundary::Async { result, .. } => Some(result.clone()),
            Boundary::Generator(_) => None,
        }
    }
    /// The body above returned or threw.
    fn finish(&self, ctx: &Ctx, completion: Completion) -> Completion {
        match self {
            Boundary::Async { settle, result } => {
                settle(ctx, completion);
                Ok(result.clone())
            }
            Boundary::Generator(generator) => crate::interp::generators::finish(ctx, generator, completion),
        }
    }
    /// The body above suspended at an await.
    fn suspended(&self) -> Completion {
        match self {
            Boundary::Async { result, .. } => Ok(result.clone()),
            Boundary::Generator(_) => Ok(Value::Undefined),
        }
    }
}

/// What a generator boundary does with a yield.
pub enum Yielded {
    /// Answer the caller with this completion; the body was stored in its generator.
    Return(Completion),
    /// Keep running the body with this request (an async generator with another request waiting).
    Continue(Input),
}

/// A body off the stack, waiting to run.
pub struct Task {
    pub fut: Fut<R<Value>>,
    pub boundary: Option<Boundary>,
}

pub struct Start {
    pub task: Task,
    pub input: Option<Input>,
}

impl Start {
    pub fn new(fut: Fut<R<Value>>) -> Start {
        Start { task: Task { fut, boundary: None }, input: None }
    }
}

type Job = Box<dyn FnOnce(&Ctx) -> Option<Start>>;

/// Interpreter steps between clock checks, and how long one slice may run before the host gets a turn.
const FUEL: u32 = 64;
const SLICE_MS: u128 = 1;

struct Parked {
    id: u64,
    promise: Obj,
    loc: Option<Loc>,
    result: Option<Value>,
}

struct HostWork {
    fut: Fut<Completion>,
    settle: Box<dyn FnOnce(&Ctx, Completion)>,
}

pub struct Runner {
    queue: RefCell<VecDeque<Job>>,
    fuel: Cell<u32>,
    slice_start: Cell<Instant>,
    pending: Cell<usize>,
    next_id: Cell<u64>,
    unhandled: RefCell<Vec<(u64, Diagnostic)>>,
    parked: RefCell<Vec<Parked>>,
    next_task: Cell<u64>,
    hosts: RefCell<Vec<HostWork>>,
    inbox: RefCell<VecDeque<Box<dyn FnOnce(&Ctx)>>>,
    signal: RefCell<Option<Signal>>,
    resume: RefCell<Option<Input>>,
    depth: Cell<usize>,
    native_depth: Cell<usize>,
    stopping: Cell<bool>,
    waker: RefCell<Option<Waker>>,
    /// The intrinsic `Promise.prototype.then`.
    pub intrinsic_then: RefCell<Option<Obj>>,
}

impl Default for Runner {
    fn default() -> Self {
        Runner {
            queue: Default::default(),
            fuel: Cell::new(FUEL),
            slice_start: Cell::new(Instant::now()),
            pending: Cell::new(0),
            next_id: Cell::new(0),
            unhandled: Default::default(),
            parked: Default::default(),
            next_task: Cell::new(0),
            hosts: Default::default(),
            inbox: Default::default(),
            signal: Default::default(),
            resume: Default::default(),
            depth: Cell::new(0),
            native_depth: Cell::new(0),
            stopping: Cell::new(false),
            waker: Default::default(),
            intrinsic_then: Default::default(),
        }
    }
}

enum Driven {
    /// The task ended or moved off the stack (parked or stored in a generator).
    Finished,
    /// Paused for host work or a slice; it continues before any other job.
    Paused(Task, bool),
}

impl Runner {
    /// Activations on the running task's stack.
    pub fn depth(&self) -> usize {
        self.depth.get()
    }

    pub fn check_depth(&self) -> R<()> {
        if self.depth.get() > MAX_CALL_DEPTH {
            return Err(range_error("Maximum call stack size exceeded"));
        }
        Ok(())
    }

    /// Counts one interpreter step; true when the slice is over and the caller should yield a slice.
    pub fn tick(&self) -> bool {
        let fuel = self.fuel.get() - 1;
        if fuel > 0 {
            self.fuel.set(fuel);
            return false;
        }
        self.fuel.set(FUEL);
        self.slice_start.get().elapsed().as_millis() >= SLICE_MS
    }

    pub fn set_signal(&self, signal: Signal) {
        *self.signal.borrow_mut() = Some(signal);
    }

    fn take_signal(&self) -> Option<Signal> {
        self.signal.borrow_mut().take()
    }

    fn restore_signal(&self, signal: Option<Signal>) {
        *self.signal.borrow_mut() = signal;
    }

    pub fn take_resume(&self) -> Option<Input> {
        self.resume.borrow_mut().take()
    }

    fn set_resume(&self, input: Option<Input>) {
        *self.resume.borrow_mut() = input;
    }

    pub fn enqueue(&self, job: impl FnOnce(&Ctx) -> Option<Start> + 'static) {
        self.queue.borrow_mut().push_back(Box::new(job));
    }

    fn waker(&self) -> Waker {
        self.waker.borrow().clone().unwrap_or_else(|| Waker::noop().clone())
    }

    /// Starts host work now; its completion settles through `settle` in a later job, in completion order.
    pub fn host(&self, ctx: &Ctx, mut fut: Fut<Completion>, settle: impl FnOnce(&Ctx, Completion) + 'static) {
        // Starts synchronously, so admission and hooks happen at the call site.
        let waker = self.waker();
        let mut cx = Context::from_waker(&waker);
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(completion) => {
                let ctx2 = ctx.clone();
                let _ = ctx2;
                self.inbox.borrow_mut().push_back(Box::new(move |ctx: &Ctx| settle(ctx, completion)));
            }
            Poll::Pending => self.hosts.borrow_mut().push(HostWork { fut, settle: Box::new(settle) }),
        }
    }

    /// Polls running host work; finished work moves to the inbox in completion order. True when any finished.
    fn poll_hosts(&self, cx: &mut Context<'_>) -> bool {
        let mut finished = false;
        let mut index = 0;
        loop {
            let work = {
                let mut hosts = self.hosts.borrow_mut();
                if index >= hosts.len() {
                    break;
                }
                // Take the future out so polling it may start more host work.
                std::mem::replace(&mut hosts[index].fut, boxed(std::future::pending()))
            };
            let mut fut = work;
            match fut.as_mut().poll(cx) {
                Poll::Ready(completion) => {
                    let done = self.hosts.borrow_mut().remove(index);
                    let settle = done.settle;
                    if !self.stopping.get() {
                        self.inbox.borrow_mut().push_back(Box::new(move |ctx: &Ctx| settle(ctx, completion)));
                    }
                    finished = true;
                }
                Poll::Pending => {
                    self.hosts.borrow_mut()[index].fut = fut;
                    index += 1;
                }
            }
        }
        finished
    }

    /// Stops host work still running.
    pub fn interrupt(&self) {
        self.stopping.set(true);
        let hosts = std::mem::take(&mut *self.hosts.borrow_mut());
        drop(hosts);
    }

    /// Rejections no handler observed, in the order their promises were created. Host work that finished before the
    /// program did settles first.
    pub fn diagnostics(&self, ctx: &Ctx) -> Vec<Diagnostic> {
        loop {
            let next = self.inbox.borrow_mut().pop_front();
            match next {
                Some(settle) => settle(ctx),
                None => break,
            }
        }
        let mut unhandled = self.unhandled.borrow().clone();
        unhandled.sort_by_key(|(id, _)| *id);
        unhandled.into_iter().map(|(_, diagnostic)| diagnostic).collect()
    }

    pub fn clear(&self) {
        self.queue.borrow_mut().clear();
        self.hosts.borrow_mut().clear();
        self.inbox.borrow_mut().clear();
        self.parked.borrow_mut().clear();
        self.resume.borrow_mut().take();
        self.signal.borrow_mut().take();
        self.intrinsic_then.borrow_mut().take();
    }

    // --- Promise abstract operations ---

    pub fn new_promise(&self, ctx: &Ctx) -> R<Obj> {
        if self.pending.get() >= MAX_PENDING_PROMISES {
            return Err(range_error(format!(
                "Too many pending promises (limit {MAX_PENDING_PROMISES}); await promises before creating more."
            )));
        }
        self.pending.set(self.pending.get() + 1);
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        Ok(ctx.new_promise_object(id))
    }

    /// CreateResolvingFunctions, for host callers.
    pub fn resolving(&self, promise: &Obj, site: Option<Loc>) -> Resolving {
        Resolving { promise: promise.clone(), done: Rc::new(Cell::new(false)), site }
    }

    /// PerformPromiseThen with one reaction for either outcome; marks the promise handled.
    pub fn then(&self, promise: &Obj, reaction: Rc<Reaction>) {
        let data = promise.promise().expect("a promise");
        match data.state.get() {
            PromiseState::Pending => data.reactions.borrow_mut().push(reaction),
            state => {
                if state == PromiseState::Rejected {
                    self.unhandled.borrow_mut().retain(|(id, _)| *id != data.id);
                }
                let settled = data.result.borrow().clone().unwrap_or(Ok(Value::Undefined));
                self.enqueue(move |ctx| reaction.run(ctx, settled));
            }
        }
        data.handled.set(true);
    }

    pub fn then_with(&self, promise: &Obj, body: impl FnOnce(&Ctx, Completion) -> Option<Start> + 'static) {
        self.then(promise, Reaction::new(body))
    }

    /// Removes a reaction an internal consumer no longer needs.
    pub fn detach(&self, promise: &Obj, reaction: &Rc<Reaction>) {
        if let Some(data) = promise.promise() {
            data.reactions.borrow_mut().retain(|item| !Rc::ptr_eq(item, reaction));
        }
    }

    /// PromiseResolve(%Promise%, value).
    pub fn promise_resolve(&self, ctx: &Ctx, value: Value) -> R<Obj> {
        if let Value::Obj(obj) = &value
            && obj.is_promise()
        {
            return Ok(obj.clone());
        }
        let promise = self.new_promise(ctx)?;
        self.resolve_with(ctx, &promise, value, None);
        Ok(promise)
    }

    pub fn settle(&self, ctx: &Ctx, promise: &Obj, completion: Completion) {
        let data = promise.promise().expect("a promise");
        if data.state.get() != PromiseState::Pending {
            return;
        }
        self.pending.set(self.pending.get() - 1);
        data.state.set(if completion.is_ok() { PromiseState::Fulfilled } else { PromiseState::Rejected });
        *data.result.borrow_mut() = Some(completion.clone());
        let reactions = std::mem::take(&mut *data.reactions.borrow_mut());
        if let Err(error) = &completion
            && !data.handled.get()
            && !error.is_interrupt()
        {
            let mut failure = crate::interp::errors::normalize_error(ctx, error);
            let mut message = crate::jsstr::JsStrBuf::new();
            message.push_str("Unhandled rejection from an un-awaited promise: ");
            message.push_js(&failure.message);
            failure.message = message.finish();
            self.unhandled.borrow_mut().push((data.id, failure));
        }
        for reaction in reactions {
            let settled = completion.clone();
            self.enqueue(move |ctx| reaction.run(ctx, settled));
        }
    }

    pub fn resolve_with(&self, ctx: &Ctx, promise: &Obj, resolution: Value, site: Option<Loc>) {
        if let Value::Obj(obj) = &resolution
            && obj.ptr_eq(promise)
        {
            let cycle = type_error("Chaining cycle detected: a promise cannot resolve with itself.").at(site);
            return self.settle(ctx, promise, Err(cycle));
        }
        let Value::Obj(thenable) = &resolution else { return self.settle(ctx, promise, Ok(resolution)) };
        let then = get(ctx, thenable, &PropKey::from_str("then")).unwrap_or(Value::Undefined);
        if !crate::interp::coerce::is_callable_value(&then) {
            return self.settle(ctx, promise, Ok(resolution));
        }
        let (promise, thenable) = (promise.clone(), thenable.clone());
        self.enqueue(move |ctx| ctx.runner.thenable_job(ctx, promise, thenable, then));
    }

    // NewPromiseResolveThenableJob. Adopting a native promise through the intrinsic `then` takes the same jobs as
    // calling it, and keeps the rejection as thrown so its diagnostic survives.
    fn thenable_job(&self, ctx: &Ctx, promise: Obj, thenable: Obj, then: Value) -> Option<Start> {
        let resolving = self.resolving(&promise, None);
        let intrinsic = self.intrinsic_then.borrow().clone();
        if let (Some(intrinsic), Value::Obj(then_obj)) = (&intrinsic, &then)
            && intrinsic.ptr_eq(then_obj)
            && thenable.is_promise()
        {
            self.then_with(&thenable, move |ctx, settled| {
                resolving.settle(ctx, settled);
                None
            });
            return None;
        }
        let resolve = {
            let resolving = resolving.clone();
            ctx.native_fn("", 1, move |ctx, _, args, _| {
                resolving.resolve(ctx, args.into_iter().next().unwrap_or_default());
                Ok(Value::Undefined)
            })
        };
        let reject = {
            let resolving = resolving.clone();
            ctx.native_fn("", 1, move |ctx, _, args, _| {
                resolving.reject(ctx, Fail::Throw(args.into_iter().next().unwrap_or_default()));
                Ok(Value::Undefined)
            })
        };
        let ctx2 = ctx.clone();
        let body = async move {
            if let Err(error) =
                ctx2.call(then, Value::Obj(thenable), vec![Value::Obj(resolve), Value::Obj(reject)], None).await
            {
                resolving.reject(&ctx2, error);
            }
            Ok(Value::Undefined)
        };
        Some(Start::new(activation(ctx, boxed(body))))
    }

    // --- Driver ---

    fn park(&self, ctx: &Ctx, task: Task, promise: Obj, loc: Option<Loc>) {
        let id = self.next_task.get();
        self.next_task.set(id + 1);
        let result = task.boundary.as_ref().and_then(|b| b.result());
        self.parked.borrow_mut().push(Parked { id, promise: promise.clone(), loc, result });
        let mut task = Some(task);
        self.then_with(&promise, move |ctx, settled| {
            ctx.runner.parked.borrow_mut().retain(|item| item.id != id);
            Some(Start { task: task.take().expect("resumed once"), input: Some(settled.into()) })
        });
        let _ = ctx;
    }

    /// Polls a task until it ends, moves off the stack, or pauses for host work or a slice.
    fn drive(&self, ctx: &Ctx, mut task: Task, input: Option<Input>, cx: &mut Context<'_>) -> R<Driven> {
        self.set_resume(input);
        loop {
            match task.fut.as_mut().poll(cx) {
                Poll::Ready(completion) => {
                    match &task.boundary {
                        Some(boundary) => {
                            let _ = boundary.finish(ctx, completion);
                        }
                        None => {
                            completion?;
                        }
                    }
                    return Ok(Driven::Finished);
                }
                Poll::Pending => match self.take_signal() {
                    Some(Signal::Await { value, loc }) => {
                        if !task.boundary.as_ref().is_some_and(|b| b.handles_await()) {
                            self.set_resume(Some(Input::Error(type_error("await is only valid in async functions."))));
                            continue;
                        }
                        match self.promise_resolve(ctx, value) {
                            Err(error) => {
                                self.set_resume(Some(Input::Error(error)));
                                continue;
                            }
                            Ok(promise) => {
                                self.park(ctx, task, promise, loc);
                                return Ok(Driven::Finished);
                            }
                        }
                    }
                    Some(Signal::Yield(value)) => {
                        let Some(Boundary::Generator(generator)) = &task.boundary else {
                            self.set_resume(Some(Input::Error(type_error("yield is only valid inside a generator."))));
                            continue;
                        };
                        let generator = generator.clone();
                        let Task { fut, boundary } = task;
                        match crate::interp::generators::yielded(ctx, &generator, value, fut) {
                            (Yielded::Continue(input), Some(fut)) => {
                                task = Task { fut, boundary };
                                self.set_resume(Some(input));
                                continue;
                            }
                            _ => return Ok(Driven::Finished),
                        }
                    }
                    Some(Signal::Slice) => return Ok(Driven::Paused(task, true)),
                    Some(Signal::Host) | None => return Ok(Driven::Paused(task, false)),
                },
            }
        }
    }

    /// Runs the program as the first job until its body completes, then returns how it ended.
    pub async fn run(&self, ctx: &Ctx, main: Fut<R<Value>>) -> R<Completion> {
        let outcome: Rc<RefCell<Option<Completion>>> = Rc::new(RefCell::new(None));
        let written = outcome.clone();
        let boundary = Boundary::Async {
            settle: Box::new(move |_, completion| *written.borrow_mut() = Some(completion)),
            result: Value::Undefined,
        };
        let mut start: Option<Start> = Some(Start { task: Task { fut: main, boundary: Some(boundary) }, input: None });
        self.slice_start.set(Instant::now());
        std::future::poll_fn(move |cx| {
            *self.waker.borrow_mut() = Some(cx.waker().clone());
            self.poll_hosts(cx);
            loop {
                if let Some(done) = outcome.borrow_mut().take() {
                    return Poll::Ready(Ok(done));
                }
                let Some(current) = start.take() else {
                    let job = self.queue.borrow_mut().pop_front();
                    if let Some(job) = job {
                        start = job(ctx);
                        if start.is_none() && self.tick() {
                            return self.yield_now(cx);
                        }
                        continue;
                    }
                    self.poll_hosts(cx);
                    let settle = self.inbox.borrow_mut().pop_front();
                    if let Some(settle) = settle {
                        settle(ctx);
                        continue;
                    }
                    if !self.hosts.borrow().is_empty() {
                        return Poll::Pending;
                    }
                    let stuck = plain_error(
                        "The program is waiting on a promise that can never settle: nothing else is running, so nothing can resolve it. Code Mode has no timers or sleep, so don't wait or poll inside a program.",
                    )
                    .at(self.stuck_await());
                    return Poll::Ready(Ok(Err(stuck)));
                };
                match self.drive(ctx, current.task, current.input, cx) {
                    Err(error) => return Poll::Ready(Err(error)),
                    Ok(Driven::Finished) => {
                        if self.tick() {
                            return self.yield_now(cx);
                        }
                    }
                    Ok(Driven::Paused(task, slice)) => {
                        start = Some(Start { task, input: None });
                        if slice {
                            return self.yield_now(cx);
                        }
                        return Poll::Pending;
                    }
                }
            }
        })
        .await
    }

    fn yield_now<T>(&self, cx: &mut Context<'_>) -> Poll<T> {
        self.slice_start.set(Instant::now());
        cx.waker().wake_by_ref();
        Poll::Pending
    }

    // The innermost stuck `await`: an outer one waits on the result of a parked async body, so prefer one that does not.
    fn stuck_await(&self) -> Option<Loc> {
        let parked = self.parked.borrow();
        let results: Vec<&Value> = parked.iter().filter_map(|item| item.result.as_ref()).collect();
        let located: Vec<&Parked> = parked.iter().filter(|item| item.loc.is_some()).collect();
        let free = located.iter().find(|item| {
            !results.iter().any(|result| matches!(result, Value::Obj(obj) if obj.ptr_eq(&item.promise)))
        });
        free.or(located.first()).and_then(|item| item.loc)
    }
}

// --- Activations and leaves ---

/// One activation on the stack: counts toward the call depth while it is polled, and grows the host stack on demand.
pub struct Activation {
    ctx: Ctx,
    fut: Fut<R<Value>>,
}

impl Future for Activation {
    type Output = R<Value>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<R<Value>> {
        let ctx = self.ctx.clone();
        let runner = &ctx.runner;
        runner.depth.set(runner.depth.get() + 1);
        // Built-ins nest on the host stack only within one activation; a program call starts afresh.
        let natives = runner.native_depth.replace(0);
        let fut = &mut self.fut;
        let result = stacker::maybe_grow(256 * 1024, 4 * 1024 * 1024, || fut.as_mut().poll(cx));
        runner.native_depth.set(natives);
        runner.depth.set(runner.depth.get() - 1);
        result
    }
}

/// How deep built-ins may nest within one activation before the host stack would overflow in TS (JavaScriptCore
/// gives up at about 3,000 levels of `toString` calling `toString`).
pub const MAX_NATIVE_DEPTH: usize = 3_000;

/// A built-in's work, counted as one level of host nesting while it is polled.
pub struct NativeWork {
    ctx: Ctx,
    fut: Fut<R<Value>>,
}

impl Future for NativeWork {
    type Output = R<Value>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<R<Value>> {
        let ctx = self.ctx.clone();
        let runner = &ctx.runner;
        runner.native_depth.set(runner.native_depth.get() + 1);
        let fut = &mut self.fut;
        let result = stacker::maybe_grow(256 * 1024, 4 * 1024 * 1024, || fut.as_mut().poll(cx));
        runner.native_depth.set(runner.native_depth.get() - 1);
        result
    }
}

impl Runner {
    /// The host stack overflow TS reaches when built-ins nest too deeply.
    pub fn check_native_depth(&self) -> R<()> {
        if self.native_depth.get() >= MAX_NATIVE_DEPTH {
            return Err(host_stack_overflow());
        }
        Ok(())
    }
}

/// JavaScriptCore's stack overflow, as a host error escaping a built-in.
pub fn host_stack_overflow() -> Fail {
    Fail::Host(Rc::new((crate::jsstr::JsStr::from("RangeError"), crate::jsstr::JsStr::from("Maximum call stack size exceeded."))))
}

pub fn native_work(ctx: &Ctx, fut: Fut<R<Value>>) -> Fut<R<Value>> {
    boxed(NativeWork { ctx: ctx.clone(), fut })
}

pub fn activation(ctx: &Ctx, fut: Fut<R<Value>>) -> Fut<R<Value>> {
    boxed(Activation { ctx: ctx.clone(), fut })
}

/// Runs `fut` as a body under `boundary` on the caller's stack: it continues until the body ends or suspends at a
/// signal the boundary handles, and propagates any other signal outward.
pub async fn drive_inline(ctx: &Ctx, fut: Fut<R<Value>>, boundary: Boundary, input: Option<Input>) -> Completion {
    let mut slot: Option<(Fut<R<Value>>, Boundary)> = Some((fut, boundary));
    let runner = &ctx.runner;
    if input.is_some() {
        runner.set_resume(input);
    }
    std::future::poll_fn(move |cx| {
        loop {
            let (fut, boundary) = slot.as_mut().expect("polled after completion");
            match fut.as_mut().poll(cx) {
                Poll::Ready(completion) => {
                    let (_, boundary) = slot.take().unwrap();
                    return Poll::Ready(boundary.finish(ctx, completion));
                }
                Poll::Pending => {
                    let signal = runner.take_signal();
                    match signal {
                        Some(Signal::Await { value, loc }) if boundary.handles_await() => {
                            match runner.promise_resolve(ctx, value) {
                                Err(error) => {
                                    runner.set_resume(Some(Input::Error(error)));
                                    continue;
                                }
                                Ok(promise) => {
                                    let (fut, boundary) = slot.take().unwrap();
                                    let suspended = boundary.suspended();
                                    runner.park(ctx, Task { fut, boundary: Some(boundary) }, promise, loc);
                                    return Poll::Ready(suspended);
                                }
                            }
                        }
                        Some(Signal::Yield(value)) if boundary.handles_yield() => {
                            let Boundary::Generator(generator) = &boundary else { unreachable!() };
                            let generator = generator.clone();
                            let (fut, boundary) = slot.take().unwrap();
                            match crate::interp::generators::yielded(ctx, &generator, value, fut) {
                                (Yielded::Continue(input), Some(fut)) => {
                                    slot = Some((fut, boundary));
                                    runner.set_resume(Some(input));
                                    continue;
                                }
                                (Yielded::Return(completion), _) => return Poll::Ready(completion),
                                (Yielded::Continue(_), None) => return Poll::Ready(Ok(Value::Undefined)),
                            }
                        }
                        other => {
                            runner.restore_signal(other);
                            return Poll::Pending;
                        }
                    }
                }
            }
        }
    })
    .await
}

/// `await value`: suspends the innermost async body until `value` settles.
pub struct AwaitLeaf {
    ctx: Ctx,
    value: Option<Value>,
    loc: Option<Loc>,
}

impl Future for AwaitLeaf {
    type Output = R<Value>;
    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<R<Value>> {
        if let Some(value) = self.value.take() {
            let loc = self.loc;
            self.ctx.runner.set_signal(Signal::Await { value, loc });
            return Poll::Pending;
        }
        match self.ctx.runner.take_resume() {
            Some(Input::Value(value)) => Poll::Ready(Ok(value)),
            Some(Input::Error(error)) => Poll::Ready(Err(error)),
            Some(Input::Request(_)) | None => Poll::Ready(Err(type_error("An await resumed without a value."))),
        }
    }
}

pub fn await_leaf(ctx: &Ctx, value: Value, loc: Option<Loc>) -> AwaitLeaf {
    AwaitLeaf { ctx: ctx.clone(), value: Some(value), loc }
}

/// `yield value`: suspends the innermost generator body; continues with the request that resumes it.
pub struct YieldLeaf {
    ctx: Ctx,
    value: Option<Value>,
}

impl Future for YieldLeaf {
    type Output = R<GenRequest>;
    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<R<GenRequest>> {
        if let Some(value) = self.value.take() {
            self.ctx.runner.set_signal(Signal::Yield(value));
            return Poll::Pending;
        }
        match self.ctx.runner.take_resume() {
            Some(Input::Request(request)) => Poll::Ready(Ok(request)),
            Some(Input::Error(error)) => Poll::Ready(Err(error)),
            Some(Input::Value(value)) => Poll::Ready(Ok(GenRequest { kind: GenRequestKind::Next, value })),
            None => Poll::Ready(Ok(GenRequest { kind: GenRequestKind::Next, value: Value::Undefined })),
        }
    }
}

pub fn yield_leaf(ctx: &Ctx, value: Value) -> YieldLeaf {
    YieldLeaf { ctx: ctx.clone(), value: Some(value) }
}

/// Lets the host run; the current job continues before any other job.
pub struct SliceLeaf {
    ctx: Ctx,
    done: bool,
}

impl Future for SliceLeaf {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        if self.done {
            return Poll::Ready(());
        }
        self.done = true;
        self.ctx.runner.set_signal(Signal::Slice);
        Poll::Pending
    }
}

pub fn slice(ctx: &Ctx) -> SliceLeaf {
    SliceLeaf { ctx: ctx.clone(), done: false }
}

/// Blocks the current job on host work: no other job runs until it ends.
pub struct BlockingLeaf<T> {
    ctx: Ctx,
    fut: Fut<T>,
}

impl<T> Future for BlockingLeaf<T> {
    type Output = T;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        match self.fut.as_mut().poll(cx) {
            Poll::Ready(value) => Poll::Ready(value),
            Poll::Pending => {
                self.ctx.runner.set_signal(Signal::Host);
                Poll::Pending
            }
        }
    }
}

pub fn blocking<T>(ctx: &Ctx, fut: Fut<T>) -> BlockingLeaf<T> {
    BlockingLeaf { ctx: ctx.clone(), fut }
}
