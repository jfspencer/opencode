//! Promises (promises.ts): capabilities, `then`/`catch`/`finally`, the statics and combinators, the constructor, and
//! the promise-like result of `search`.

use crate::interp::callback::is_supported_callback;
use crate::interp::coerce::{describe_value, is_callable_value};
use crate::interp::errors::{Fail, materialize, type_error};
use crate::interp::native::{NativeOptions, receiver, requires_new};
use crate::interp::object::define;
use crate::interp::runner::{Completion, Reaction, Resolving, Start};
use crate::interp::value::*;
use crate::interp::{Ctx, Loc, R, boxed};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// NewPromiseCapability(%Promise%): a pending promise with the host functions that settle it once.
pub fn capability(ctx: &Ctx, site: Option<Loc>) -> R<Resolving> {
    let promise = ctx.runner.new_promise(ctx)?;
    Ok(ctx.runner.resolving(&promise, site))
}

// A `resolve`/`reject` handed to an executor or thenable: calling it settles the capability.
fn resolving_functions(ctx: &Ctx, resolving: &Resolving) -> (Obj, Obj) {
    let resolve = {
        let resolving = resolving.clone();
        ctx.native_fn("resolve", 1, move |ctx, _, args, _| {
            resolving.resolve(ctx, args.into_iter().next().unwrap_or_default());
            Ok(Value::Undefined)
        })
    };
    let reject = {
        let resolving = resolving.clone();
        ctx.native_fn("reject", 1, move |ctx, _, args, _| {
            resolving.reject(ctx, Fail::Throw(args.into_iter().next().unwrap_or_default()));
            Ok(Value::Undefined)
        })
    };
    (resolve, reject)
}

/// A promise reaction that calls program code: the handler runs as a job, and its result or failure settles the
/// derived promise. A missing handler passes the outcome through.
fn handler_reaction(
    on_fulfilled: Option<Value>,
    on_rejected: Option<Value>,
    derived: Option<Resolving>,
) -> impl FnOnce(&Ctx, Completion) -> Option<Start> + 'static {
    move |ctx, settled| {
        let handler = if settled.is_ok() { on_fulfilled } else { on_rejected };
        let interrupted = matches!(&settled, Err(error) if error.is_interrupt());
        let Some(handler) = handler.filter(|_| !interrupted) else {
            if let Some(derived) = derived {
                derived.settle(ctx, settled);
            }
            return None;
        };
        let argument = match &settled {
            Ok(value) => value.clone(),
            Err(error) => materialize(ctx, error),
        };
        let ctx2 = ctx.clone();
        Some(ctx.start_task(async move {
            match ctx2.call(handler, Value::Undefined, vec![argument], None).await {
                Err(error) => {
                    if let Some(derived) = &derived {
                        derived.reject(&ctx2, error);
                    }
                }
                Ok(result) => {
                    if let Some(derived) = &derived {
                        derived.resolve(&ctx2, result);
                    }
                }
            }
            Ok(Value::Undefined)
        }))
    }
}

const STATICS: [&str; 8] = ["all", "allSettled", "race", "any", "resolve", "reject", "withResolvers", "try"];

async fn invoke_static(ctx: Ctx, name: &'static str, args: Vec<Value>, site: Option<Loc>) -> R<Value> {
    let runner = &ctx.runner;
    let first = args.first().cloned().unwrap_or_default();
    match name {
        "resolve" => return Ok(Value::Obj(runner.promise_resolve(&ctx, first)?)),
        "reject" => {
            let made = capability(&ctx, site)?;
            made.reject(&ctx, Fail::Throw(first));
            return Ok(Value::Obj(made.promise().clone()));
        }
        "withResolvers" => {
            let made = capability(&ctx, site)?;
            let (resolve, reject) = resolving_functions(&ctx, &made);
            return Ok(Value::Obj(ctx.record(vec![
                ("promise", Value::Obj(made.promise().clone())),
                ("resolve", Value::Obj(resolve)),
                ("reject", Value::Obj(reject)),
            ])));
        }
        "try" => {
            let made = capability(&ctx, site)?;
            // Calling something that is not a function fails like the callback throwing: the promise rejects.
            let outcome = async {
                if !is_callable_value(&first) {
                    return Err(type_error(format!("Promise.try expects a function, received {}.", describe_value(&first))));
                }
                ctx.call(first.clone(), Value::Undefined, args[1..].to_vec(), None).await
            }
            .await;
            match outcome {
                Ok(value) => made.resolve(&ctx, value),
                Err(error) => made.reject(&ctx, error),
            }
            return Ok(Value::Obj(made.promise().clone()));
        }
        _ => {}
    }
    combinator(&ctx, name, first, site).await
}

// Promise.all, allSettled, race, and any: each element is resolved to a promise and observed through a reaction, so
// the result settles in the job where the deciding element's reaction runs, or at once for an empty input.
async fn combinator(ctx: &Ctx, name: &'static str, iterable: Value, site: Option<Loc>) -> R<Value> {
    let runner = &ctx.runner;
    let made = capability(ctx, site)?;
    struct State {
        values: RefCell<Vec<Value>>,
        reasons: RefCell<Vec<Value>>,
        attached: RefCell<Vec<(Obj, Rc<Reaction>)>>,
        remaining: Cell<usize>,
    }
    let state = Rc::new(State {
        values: Default::default(),
        reasons: Default::default(),
        attached: Default::default(),
        remaining: Cell::new(1),
    });
    let finish = {
        let (state, made) = (state.clone(), made.clone());
        move |ctx: &Ctx| {
            let remaining = state.remaining.get() - 1;
            state.remaining.set(remaining);
            if remaining > 0 {
                return;
            }
            if name == "any" {
                let errors = state.reasons.borrow().clone();
                let aggregate = crate::stdlib::errors::aggregate_error(ctx, errors, "All promises were rejected", None);
                made.reject(ctx, Fail::Throw(Value::Obj(aggregate)));
                return;
            }
            made.resolve(ctx, Value::Obj(ctx.new_array(state.values.borrow().clone())));
        }
    };
    let finish = Rc::new(finish);
    let outcome: R<()> = async {
        let Some(cursor) = ctx.iterate(&iterable, None).await? else {
            return Err(type_error(format!(
                "Promise.{name} expects a synchronous iterable, received {}.",
                describe_value(&iterable)
            )));
        };
        let mut index = 0;
        loop {
            let step = cursor.next_step(ctx).await?;
            if step.done {
                break;
            }
            let item = runner.promise_resolve(ctx, step.value)?;
            let at = index;
            index += 1;
            state.values.borrow_mut().push(Value::Undefined);
            state.reasons.borrow_mut().push(Value::Undefined);
            state.remaining.set(state.remaining.get() + 1);
            let reaction = {
                let (state, made, finish) = (state.clone(), made.clone(), finish.clone());
                Reaction::new(move |ctx, settled| {
                    // Once the result settles early, the remaining members' reactions can do nothing: detach them.
                    let decided = |ctx: &Ctx, settle: &dyn Fn(&Ctx)| {
                        settle(ctx);
                        for (member, reaction) in state.attached.borrow().iter() {
                            ctx.runner.detach(member, reaction);
                        }
                    };
                    let interrupted = matches!(&settled, Err(error) if error.is_interrupt());
                    if name == "race" || interrupted {
                        decided(ctx, &|ctx| made.settle(ctx, settled.clone()));
                        return None;
                    }
                    match (name, &settled) {
                        ("all", Err(error)) => decided(ctx, &|ctx| made.reject(ctx, error.clone())),
                        ("all", Ok(value)) => {
                            state.values.borrow_mut()[at] = value.clone();
                            finish(ctx);
                        }
                        ("any", Ok(value)) => decided(ctx, &|ctx| made.resolve(ctx, value.clone())),
                        ("any", Err(error)) => {
                            state.reasons.borrow_mut()[at] = materialize(ctx, error);
                            finish(ctx);
                        }
                        (_, settled) => {
                            let record = match settled {
                                Ok(value) => ctx.record(vec![("status", Value::str("fulfilled")), ("value", value.clone())]),
                                Err(error) => {
                                    ctx.record(vec![("status", Value::str("rejected")), ("reason", materialize(ctx, error))])
                                }
                            };
                            state.values.borrow_mut()[at] = Value::Obj(record);
                            finish(ctx);
                        }
                    }
                    None
                })
            };
            state.attached.borrow_mut().push((item.clone(), reaction.clone()));
            runner.then(&item, reaction);
        }
        if name == "race" && index == 0 {
            return Err(type_error("Promise.race([]) would never settle; provide at least one promise or value."));
        }
        finish(ctx);
        Ok(())
    }
    .await;
    if let Err(error) = outcome {
        made.reject(ctx, error);
    }
    Ok(Value::Obj(made.promise().clone()))
}

fn reaction_handler(value: &Value, method: &str) -> R<Option<Value>> {
    if is_supported_callback(value) {
        return Ok(Some(value.clone()));
    }
    if is_callable_value(value) {
        return Err(type_error(format!(
            "{method} cannot use this callable as a handler; wrap it in an arrow function, e.g. (value) => tools.ns.tool(value)."
        )));
    }
    Ok(None)
}

fn instance_method(ctx: &Ctx, name: &str, this: &Value, args: &[Value], site: Option<Loc>) -> R<Value> {
    let method = format!("Promise.prototype.{name}");
    let promise = receiver(this, |obj| obj.is_promise(), &method)?;
    let arg = |index: usize| args.get(index).cloned().unwrap_or_default();
    if name == "finally" {
        return chain_finally(ctx, &promise, reaction_handler(&arg(0), &method)?, site);
    }
    let on_fulfilled = if name == "then" { reaction_handler(&arg(0), &method)? } else { None };
    let on_rejected = reaction_handler(&if name == "then" { arg(1) } else { arg(0) }, &method)?;
    let made = capability(ctx, site)?;
    ctx.runner.then_with(&promise, handler_reaction(on_fulfilled, on_rejected, Some(made.clone())));
    Ok(Value::Obj(made.promise().clone()))
}

// Promise.prototype.finally: the cleanup runs as a reaction, then the original outcome passes through once the
// cleanup's own result settles; a failing cleanup wins.
fn chain_finally(ctx: &Ctx, source: &Obj, cleanup: Option<Value>, site: Option<Loc>) -> R<Value> {
    let runner = &ctx.runner;
    let made = capability(ctx, site)?;
    let Some(cleanup) = cleanup else {
        let made2 = made.clone();
        runner.then_with(source, move |ctx, settled| {
            made2.settle(ctx, settled);
            None
        });
        return Ok(Value::Obj(made.promise().clone()));
    };
    let made2 = made.clone();
    runner.then_with(source, move |ctx, settled| {
        if let Err(error) = &settled
            && error.is_interrupt()
        {
            made2.reject(ctx, error.clone());
            return None;
        }
        let ctx2 = ctx.clone();
        Some(ctx.start_task(async move {
            let result = match ctx2.call(cleanup, Value::Undefined, Vec::new(), None).await {
                Err(error) => {
                    made2.reject(&ctx2, error);
                    return Ok(Value::Undefined);
                }
                Ok(result) => result,
            };
            let after = match ctx2.runner.promise_resolve(&ctx2, result) {
                Err(error) => {
                    made2.reject(&ctx2, error);
                    return Ok(Value::Undefined);
                }
                Ok(after) => after,
            };
            // The spec chains `then(valueThunk)` onto the cleanup's promise: one more job before the outcome passes.
            let derived = capability(&ctx2, site)?;
            let derived2 = derived.clone();
            ctx2.runner.then_with(&after, move |ctx, cleaned| {
                match cleaned {
                    Ok(_) => derived2.resolve(ctx, Value::Undefined),
                    Err(error) => derived2.reject(ctx, error),
                }
                None
            });
            ctx2.runner.then_with(derived.promise(), move |ctx, cleaned| {
                made2.settle(ctx, if cleaned.is_ok() { settled } else { cleaned });
                None
            });
            Ok(Value::Undefined)
        }))
    });
    Ok(Value::Obj(made.promise().clone()))
}

async fn construct_promise(ctx: Ctx, executor: Value, site: Option<Loc>) -> R<Value> {
    if !matches!(&executor, Value::Obj(obj) if obj.program_fn().is_some()) {
        return Err(type_error(
            "new Promise(...) expects an executor function (e.g. new Promise((resolve, reject) => { ... })).",
        ));
    }
    let made = capability(&ctx, site)?;
    let (resolve, reject) = resolving_functions(&ctx, &made);
    if let Err(error) = ctx.call(executor, Value::Undefined, vec![Value::Obj(resolve), Value::Obj(reject)], None).await {
        made.reject(&ctx, error);
    }
    Ok(Value::Obj(made.promise().clone()))
}

/// Lets a synchronous object result also be awaited or chained like a Promise.
pub fn promise_like(ctx: &Ctx, value: Value) -> Value {
    let Value::Obj(obj) = &value else { return value };
    if !obj.proto().is_some_and(|proto| proto.ptr_eq(&ctx.builtins.object)) {
        return value;
    }
    obj.set_proto(Some(promise_like_prototype(ctx)));
    value
}

fn promise_like_prototype(ctx: &Ctx) -> Obj {
    if let Some(cached) = ctx.promise_like.borrow().clone() {
        return cached;
    }
    let proto = ctx.new_plain();
    for (name, length) in [("then", 2), ("catch", 1), ("finally", 1)] {
        ctx.method(&proto, name, length, move |ctx, this, args, site| {
            let source = receiver(&this, |_| true, name)?;
            let copy = ctx.new_plain();
            {
                let props = source.0.props.borrow();
                let mut target = copy.0.props.borrow_mut();
                for (key, slot) in props.iter() {
                    target.insert(key.clone(), slot.clone());
                }
            }
            let settled = ctx.runner.promise_resolve(ctx, Value::Obj(copy))?;
            instance_method(ctx, name, &Value::Obj(settled), &args, site)
        });
    }
    *ctx.promise_like.borrow_mut() = Some(proto.clone());
    proto
}

pub fn promise_global(ctx: &Ctx) -> Value {
    let builtins = &ctx.builtins;
    let proto = builtins.promise.clone();
    let promise = ctx.constructor(
        &proto,
        NativeOptions {
            name: "Promise".into(),
            length: 1,
            call: requires_new("Promise"),
            construct: Some(Rc::new(|ctx, args, _, site| {
                let executor = args.into_iter().next().unwrap_or_default();
                Ok(NOut::Work(boxed(construct_promise(ctx.clone(), executor, site))))
            })),
            callback: true,
        },
    );
    // Combinators are not callbacks: `[p].map(Promise.resolve)` must ask for an arrow function.
    for name in STATICS {
        let function = ctx.new_native(NativeOptions {
            name: name.into(),
            length: 1,
            call: Rc::new(move |ctx, _, args, site| Ok(NOut::Work(boxed(invoke_static(ctx.clone(), name, args, site))))),
            construct: None,
            callback: false,
        });
        define(&promise, PropKey::from_str(name), Value::Obj(function), HIDDEN);
    }
    for (name, length) in [("then", 2), ("catch", 1), ("finally", 1)] {
        ctx.method(&proto, name, length, move |ctx, this, args, site| instance_method(ctx, name, &this, &args, site));
    }
    if let Value::Obj(then) = crate::interp::object::get_data(&proto, &PropKey::from_str("then")) {
        *ctx.runner.intrinsic_then.borrow_mut() = Some(then);
    }
    Value::Obj(promise)
}
