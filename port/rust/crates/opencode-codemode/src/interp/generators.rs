//! Generator objects (generators.ts): `next`/`return`/`throw` on the generator prototypes, the async generator
//! request queue, and the generator boundary's handling of a body's end and its yields.

use crate::interp::errors::{Fail, type_error};
use crate::interp::native::receiver_generator;
use crate::interp::runner::{Boundary, Completion, Input, Yielded, drive_inline};
use crate::interp::value::*;
use crate::interp::{Ctx, Fut, R, boxed};
use std::rc::Rc;

pub fn install(ctx: &Ctx) {
    let builtins = &ctx.builtins;
    for kind in [GenRequestKind::Next, GenRequestKind::Return, GenRequestKind::Throw] {
        let name = kind.as_str();
        let sync_kind = kind.clone();
        ctx.method_work(&builtins.generator, name, 1, move |ctx, this, args, _| {
            let method = format!("Generator.prototype.{}", sync_kind.as_str());
            let generator = receiver_generator(&this, &method)?;
            if generator.generator().is_some_and(|g| g.asynchronous) {
                return Err(type_error(format!("{method} called on incompatible receiver an async generator.")));
            }
            let (ctx, kind, value) = (ctx.clone(), sync_kind.clone(), args.into_iter().next().unwrap_or_default());
            Ok(NOut::Work(boxed(async move { step(&ctx, &generator, kind, value).await })))
        });
        let async_kind = kind.clone();
        ctx.method_work(&builtins.async_generator, name, 1, move |ctx, this, args, _| {
            let (ctx, kind, value) = (ctx.clone(), async_kind.clone(), args.into_iter().next().unwrap_or_default());
            Ok(NOut::Work(boxed(async move { request(&ctx, this, kind, value).await })))
        });
    }
    let iterator_self = ctx.native_fn("[Symbol.iterator]", 0, |_, this, _, _| Ok(this));
    crate::interp::object::define(&builtins.iterator, PropKey::Sym(Sym::Iterator), Value::Obj(iterator_self), HIDDEN);
    let async_self = ctx.native_fn("[Symbol.asyncIterator]", 0, |_, this, _, _| Ok(this));
    crate::interp::object::define(&builtins.async_iterator, PropKey::Sym(Sym::AsyncIterator), Value::Obj(async_self), HIDDEN);
}

fn result(ctx: &Ctx, value: Value, done: bool) -> Value {
    Value::Obj(ctx.iter_result(value, done))
}

fn data(generator: &Obj) -> &GenData {
    generator.generator().expect("a generator")
}

/// Puts the suspended body back on the stack and continues it with `request`.
async fn resume(ctx: &Ctx, generator: &Obj, request: GenRequest) -> Completion {
    let gen_data = data(generator);
    let started = gen_data.state.get() != GenState::SuspendedStart;
    gen_data.state.set(GenState::Executing);
    let body = gen_data.body.borrow_mut().take().expect("a suspended body");
    let input = if started { Some(Input::Request(request)) } else { None };
    drive_inline(ctx, body, Boundary::Generator(generator.clone()), input).await
}

/// A synchronous generator step: the body runs on the caller's stack until it yields, returns, or throws.
pub async fn step(ctx: &Ctx, generator: &Obj, kind: GenRequestKind, value: Value) -> R<Value> {
    let gen_data = data(generator);
    if gen_data.state.get() == GenState::Executing {
        return Err(type_error("Generator is already running."));
    }
    if gen_data.state.get() == GenState::SuspendedStart && !matches!(kind, GenRequestKind::Next) {
        gen_data.state.set(GenState::Completed);
    }
    if gen_data.state.get() == GenState::Completed {
        return match kind {
            GenRequestKind::Throw => Err(Fail::Throw(value)),
            GenRequestKind::Return => Ok(result(ctx, value, true)),
            GenRequestKind::Next => Ok(result(ctx, Value::Undefined, true)),
        };
    }
    resume(ctx, generator, GenRequest { kind, value }).await
}

// AsyncGenerator.prototype.next/return/throw: queue the request with its promise, and run the body now if it is
// waiting for one.
async fn request(ctx: &Ctx, this: Value, kind: GenRequestKind, value: Value) -> R<Value> {
    let runner = &ctx.runner;
    let promise = runner.new_promise(ctx)?;
    let resolving = runner.resolving(&promise, None);
    let generator = match &this {
        Value::Obj(obj) if obj.generator().is_some_and(|g| g.asynchronous) => obj.clone(),
        _ => {
            resolving.reject(
                ctx,
                type_error(format!("AsyncGenerator.prototype.{} called on incompatible receiver.", kind.as_str())),
            );
            return Ok(Value::Obj(promise));
        }
    };
    let gen_data = data(&generator);
    let state = gen_data.state.get();
    if matches!(kind, GenRequestKind::Next) && state == GenState::Completed {
        resolving.resolve(ctx, result(ctx, Value::Undefined, true));
        return Ok(Value::Obj(promise));
    }
    if matches!(kind, GenRequestKind::Throw) {
        if state == GenState::SuspendedStart {
            gen_data.state.set(GenState::Completed);
        }
        if gen_data.state.get() == GenState::Completed {
            resolving.reject(ctx, Fail::Throw(value));
            return Ok(Value::Obj(promise));
        }
    }
    let queued = GenRequest { kind: kind.clone(), value };
    gen_data.queue.borrow_mut().push_back((queued.clone(), resolving));
    if matches!(kind, GenRequestKind::Return) && (state == GenState::SuspendedStart || state == GenState::Completed) {
        gen_data.state.set(GenState::AwaitingReturn);
        await_return(ctx, &generator);
        return Ok(Value::Obj(promise));
    }
    if state == GenState::SuspendedStart || state == GenState::SuspendedYield {
        let _ = resume(ctx, &generator, queued).await;
    }
    Ok(Value::Obj(promise))
}

// AsyncGeneratorCompleteStep: answer the oldest request.
fn complete_step(ctx: &Ctx, generator: &Obj, completion: Completion, done: bool) {
    let next = data(generator).queue.borrow_mut().pop_front();
    let Some((_, resolving)) = next else { return };
    match completion {
        Ok(value) => resolving.resolve(ctx, result(ctx, value, done)),
        Err(error) => resolving.reject(ctx, error),
    }
}

// AsyncGeneratorDrainQueue: once the body has ended, answer the remaining requests in order.
fn drain_queue(ctx: &Ctx, generator: &Obj) {
    let gen_data = data(generator);
    gen_data.state.set(GenState::Draining);
    loop {
        let next = gen_data.queue.borrow().front().map(|(request, _)| request.clone());
        let Some(next) = next else {
            gen_data.state.set(GenState::Completed);
            return;
        };
        match next.kind {
            GenRequestKind::Return => {
                gen_data.state.set(GenState::AwaitingReturn);
                return await_return(ctx, generator);
            }
            GenRequestKind::Throw => complete_step(ctx, generator, Err(Fail::Throw(next.value)), true),
            GenRequestKind::Next => complete_step(ctx, generator, Ok(Value::Undefined), true),
        }
    }
}

// AsyncGeneratorAwaitReturn: a `return(value)` on an ended or unstarted generator settles once `value` does.
fn await_return(ctx: &Ctx, generator: &Obj) {
    let runner = &ctx.runner;
    let value = data(generator).queue.borrow().front().map(|(request, _)| request.value.clone()).unwrap_or_default();
    let finish = {
        let generator = generator.clone();
        move |ctx: &Ctx, completion: Completion| {
            complete_step(ctx, &generator, completion, true);
            drain_queue(ctx, &generator);
        }
    };
    match runner.promise_resolve(ctx, value) {
        Ok(promise) => runner.then_with(&promise, move |ctx, settled| {
            finish(ctx, settled);
            None
        }),
        Err(error) => finish(ctx, Err(error)),
    }
}

/// The generator boundary: the body above returned or threw.
pub fn finish(ctx: &Ctx, generator: &Obj, completion: Completion) -> Completion {
    let gen_data = data(generator);
    if !gen_data.asynchronous {
        gen_data.state.set(GenState::Completed);
        return completion.map(|value| result(ctx, value, true));
    }
    complete_step(ctx, generator, completion, true);
    drain_queue(ctx, generator);
    Ok(Value::Undefined)
}

/// The generator boundary: the body above yielded `value`. Hands the body back when it should keep running.
pub fn yielded(ctx: &Ctx, generator: &Obj, value: Value, body: Fut<R<Value>>) -> (Yielded, Option<Fut<R<Value>>>) {
    let gen_data = data(generator);
    if !gen_data.asynchronous {
        *gen_data.body.borrow_mut() = Some(body);
        gen_data.state.set(GenState::SuspendedYield);
        return (Yielded::Return(Ok(result(ctx, value, false))), None);
    }
    // AsyncGeneratorYield: answer the oldest request, and keep running if another is already waiting.
    complete_step(ctx, generator, Ok(value), false);
    let next = gen_data.queue.borrow().front().map(|(request, _)| request.clone());
    if let Some(next) = next {
        return (Yielded::Continue(Input::Request(next)), Some(body));
    }
    *gen_data.body.borrow_mut() = Some(body);
    gen_data.state.set(GenState::SuspendedYield);
    (Yielded::Return(Ok(Value::Undefined)), None)
}

pub fn new_generator(ctx: &Ctx, proto: Obj, asynchronous: bool, body: Fut<R<Value>>) -> Obj {
    ctx.new_object(
        Some(proto),
        Kind::Generator(GenData {
            asynchronous,
            state: std::cell::Cell::new(GenState::SuspendedStart),
            body: std::cell::RefCell::new(Some(body)),
            queue: Default::default(),
        }),
    )
}

pub type _Unused = Rc<()>;
