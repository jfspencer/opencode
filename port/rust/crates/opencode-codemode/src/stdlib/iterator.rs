//! `Iterator` and the iterator helpers (iterator.ts): lazy, one source step per result, closing the source on early
//! exit or when a callback throws.

use crate::interp::callback::{Apply, apply_collection_callback};
use crate::interp::coerce::{describe_value, to_number_plain};
use crate::interp::cursor::{Cursor, DynCursor, Pull, Step};
use crate::interp::errors::{range_error, type_error};
use crate::interp::native::{NativeOptions, receiver, requires_new};
use crate::interp::object::{define, has_prototype};
use crate::interp::value::*;
use crate::interp::{Ctx, Fut, R, boxed};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

fn method(name: &str) -> String {
    format!("Iterator.prototype.{name}")
}

/// A step that runs program code: when it throws, close the source first.
async fn guarded<T>(ctx: &Ctx, source: &Rc<Cursor>, body: R<T>) -> R<T> {
    match body {
        Ok(value) => Ok(value),
        Err(error) => {
            if error.is_interrupt() {
                return Err(error);
            }
            let _ = source.close_now(ctx).await;
            Err(error)
        }
    }
}

type PullFn = Rc<dyn Fn(Ctx) -> Fut<R<Step>>>;
type CloseFn = Rc<dyn Fn(Ctx) -> Fut<R<()>>>;

/// A lazy helper: once it reports done, throws, or is closed, it stays done. A callback that re-enters its own
/// helper is a TypeError, as for a running generator.
struct Helper {
    pull: RefCell<Option<PullFn>>,
    close: RefCell<Option<CloseFn>>,
    done: Cell<bool>,
    running: Cell<bool>,
}

impl Helper {
    fn enter(&self) -> R<()> {
        if self.running.get() {
            return Err(type_error("Iterator helper is already running."));
        }
        self.running.set(true);
        Ok(())
    }
}

impl DynCursor for Helper {
    fn next(self: Rc<Self>, ctx: &Ctx) -> Pull<Step> {
        if self.done.get() {
            return Pull::Ready(Ok(Step::done()));
        }
        if let Err(error) = self.enter() {
            return Pull::Ready(Err(error));
        }
        let Some(pull) = self.pull.borrow().clone() else { return Pull::Ready(Ok(Step::done())) };
        let ctx = ctx.clone();
        Pull::Work(boxed(async move {
            let result = pull(ctx).await;
            self.running.set(false);
            match &result {
                Ok(step) => self.done.set(step.done),
                Err(_) => self.done.set(true),
            }
            result
        }))
    }
    fn close(self: Rc<Self>, ctx: &Ctx) -> Pull<()> {
        if self.done.get() {
            return Pull::Ready(Ok(()));
        }
        if let Err(error) = self.enter() {
            return Pull::Ready(Err(error));
        }
        self.done.set(true);
        let Some(close) = self.close.borrow().clone() else {
            self.running.set(false);
            return Pull::Ready(Ok(()));
        };
        let ctx = ctx.clone();
        Pull::Work(boxed(async move {
            let result = close(ctx).await;
            self.running.set(false);
            result
        }))
    }
    fn clear(&self) {
        self.pull.borrow_mut().take();
        self.close.borrow_mut().take();
    }
}

fn helper(ctx: &Ctx, source: Rc<Cursor>, pull: PullFn, close: Option<CloseFn>) -> Value {
    let close = close.unwrap_or_else(|| {
        let source = source.clone();
        Rc::new(move |ctx: Ctx| {
            let source = source.clone();
            boxed(async move { source.close_now(&ctx).await })
        })
    });
    let state = Rc::new(Helper {
        pull: RefCell::new(Some(pull)),
        close: RefCell::new(Some(close)),
        done: Cell::new(false),
        running: Cell::new(false),
    });
    let cursor = Cursor::dynamic(state);
    Value::Obj(ctx.new_object(Some(ctx.builtins.iterator_helper.clone()), Kind::Iterator(cursor)))
}

// GetIteratorFlattenable: an iterable by `[Symbol.iterator]`, otherwise the object itself as an iterator.
async fn flattenable(ctx: &Ctx, value: &Value, name: &str, strings: bool) -> R<Rc<Cursor>> {
    let allowed = match value {
        Value::Str(_) => strings,
        Value::Obj(_) => true,
        _ => false,
    };
    if !allowed {
        return Err(type_error(format!("{name} expects an iterable or iterator, received {}.", describe_value(value))));
    }
    match ctx.iterate(value, None).await? {
        Some(cursor) => Ok(cursor),
        None => ctx.iterate_direct(value, None),
    }
}

fn limit(value: &Value, name: &str) -> R<f64> {
    let count = to_number_plain(value).trunc();
    if count.is_nan() || count < 0.0 {
        let shown = crate::numconv::to_string(count);
        return Err(range_error(format!("{} expects a non-negative count, received {shown}.", method(name))));
    }
    Ok(count)
}

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or_default()
}

fn mapper(ctx: &Ctx, source: Rc<Cursor>, apply: Apply, filter: bool) -> PullFn {
    let counter = Rc::new(Cell::new(0usize));
    Rc::new(move |ctx: Ctx| {
        let (source, apply, counter) = (source.clone(), apply.clone(), counter.clone());
        boxed(async move {
            loop {
                let step = source.next_step(&ctx).await?;
                if step.done {
                    return Ok(Step::done());
                }
                let index = counter.get();
                counter.set(index + 1);
                let result = apply(vec![step.value.clone(), Value::Num(index as f64)], Value::Undefined).await;
                let result = guarded(&ctx, &source, result).await?;
                if !filter {
                    return Ok(Step::of(result));
                }
                if result.truthy() {
                    return Ok(step);
                }
            }
        })
    })
    .tap(ctx)
}

trait Tap {
    fn tap(self, ctx: &Ctx) -> Self;
}

impl<T> Tap for T {
    fn tap(self, _: &Ctx) -> Self {
        self
    }
}

pub fn iterator_global(ctx: &Ctx) -> Value {
    let proto = ctx.builtins.iterator.clone();
    ctx.method_work(&proto, "next", 0, |ctx, this, _, _| {
        let target = receiver(&this, |obj| matches!(obj.kind(), Kind::Iterator(_)), &method("next"))?;
        let Kind::Iterator(cursor) = target.kind() else { unreachable!() };
        let cursor = cursor.clone();
        let ctx = ctx.clone();
        Ok(NOut::Work(boxed(async move {
            let step = cursor.next_step(&ctx).await?;
            Ok(Value::Obj(ctx.iter_result(step.value, step.done)))
        })))
    });
    for name in ["map", "filter"] {
        ctx.method(&proto, name, 1, move |ctx, this, args, _| {
            let source = ctx.iterate_direct(&this, None)?;
            let apply = apply_collection_callback(ctx, &arg(&args, 0), &method(name))?;
            Ok(helper(ctx, source.clone(), mapper(ctx, source, apply, name == "filter"), None))
        });
    }
    ctx.method(&proto, "take", 1, |ctx, this, args, _| {
        let source = ctx.iterate_direct(&this, None)?;
        let remaining = Rc::new(Cell::new(limit(&arg(&args, 0), "take")?));
        let pull: PullFn = {
            let source = source.clone();
            Rc::new(move |ctx: Ctx| {
                let (source, remaining) = (source.clone(), remaining.clone());
                boxed(async move {
                    if remaining.get() == 0.0 {
                        source.close_now(&ctx).await?;
                        return Ok(Step::done());
                    }
                    remaining.set(remaining.get() - 1.0);
                    source.next_step(&ctx).await
                })
            })
        };
        Ok(helper(ctx, source, pull, None))
    });
    ctx.method(&proto, "drop", 1, |ctx, this, args, _| {
        let source = ctx.iterate_direct(&this, None)?;
        let remaining = Rc::new(Cell::new(limit(&arg(&args, 0), "drop")?));
        let pull: PullFn = {
            let source = source.clone();
            Rc::new(move |ctx: Ctx| {
                let (source, remaining) = (source.clone(), remaining.clone());
                boxed(async move {
                    while remaining.get() > 0.0 {
                        remaining.set(remaining.get() - 1.0);
                        let step = source.next_step(&ctx).await?;
                        if step.done {
                            return Ok(Step::done());
                        }
                    }
                    source.next_step(&ctx).await
                })
            })
        };
        Ok(helper(ctx, source, pull, None))
    });
    ctx.method(&proto, "flatMap", 1, |ctx, this, args, _| {
        let source = ctx.iterate_direct(&this, None)?;
        let apply = apply_collection_callback(ctx, &arg(&args, 0), &method("flatMap"))?;
        let inner: Rc<RefCell<Option<Rc<Cursor>>>> = Rc::new(RefCell::new(None));
        let counter = Rc::new(Cell::new(0usize));
        let pull: PullFn = {
            let (source, inner) = (source.clone(), inner.clone());
            Rc::new(move |ctx: Ctx| {
                let (source, inner, apply, counter) = (source.clone(), inner.clone(), apply.clone(), counter.clone());
                boxed(async move {
                    loop {
                        let current = inner.borrow().clone();
                        if let Some(current) = current {
                            let step = current.next_step(&ctx).await;
                            let step = guarded(&ctx, &source, step).await?;
                            if !step.done {
                                return Ok(step);
                            }
                            inner.borrow_mut().take();
                        }
                        let step = source.next_step(&ctx).await?;
                        if step.done {
                            return Ok(Step::done());
                        }
                        let index = counter.get();
                        counter.set(index + 1);
                        let mapped = apply(vec![step.value, Value::Num(index as f64)], Value::Undefined).await;
                        let mapped = guarded(&ctx, &source, mapped).await?;
                        let next = flattenable(&ctx, &mapped, &method("flatMap"), false).await;
                        *inner.borrow_mut() = Some(guarded(&ctx, &source, next).await?);
                    }
                })
            })
        };
        let close: CloseFn = {
            let source = source.clone();
            Rc::new(move |ctx: Ctx| {
                let (source, inner) = (source.clone(), inner.clone());
                boxed(async move {
                    let current = inner.borrow().clone();
                    if let Some(current) = current {
                        current.close_now(&ctx).await?;
                    }
                    source.close_now(&ctx).await
                })
            })
        };
        Ok(helper(ctx, source, pull, Some(close)))
    });
    ctx.method_work(&proto, "reduce", 1, |ctx, this, args, _| {
        let source = ctx.iterate_direct(&this, None)?;
        let apply = apply_collection_callback(ctx, &arg(&args, 0), &method("reduce"))?;
        let ctx = ctx.clone();
        Ok(NOut::Work(boxed(async move {
            let mut counter = 0usize;
            let mut accumulator = arg(&args, 1);
            if args.len() < 2 {
                let first = source.next_step(&ctx).await?;
                if first.done {
                    return Err(type_error("Iterator.prototype.reduce of an empty iterator with no initial value."));
                }
                accumulator = first.value;
                counter = 1;
            }
            loop {
                let step = source.next_step(&ctx).await?;
                if step.done {
                    return Ok(accumulator);
                }
                let result = apply(vec![accumulator, step.value, Value::Num(counter as f64)], Value::Undefined).await;
                counter += 1;
                accumulator = guarded(&ctx, &source, result).await?;
            }
        })))
    });
    ctx.method_work(&proto, "toArray", 0, |ctx, this, _, _| {
        let source = ctx.iterate_direct(&this, None)?;
        let ctx = ctx.clone();
        Ok(NOut::Work(boxed(async move {
            let mut items = Vec::new();
            loop {
                let step = source.next_step(&ctx).await?;
                if step.done {
                    return Ok(Value::Obj(ctx.new_array(items)));
                }
                items.push(step.value);
            }
        })))
    });
    for name in ["forEach", "some", "every", "find"] {
        ctx.method_work(&proto, name, 1, move |ctx, this, args, _| {
            let source = ctx.iterate_direct(&this, None)?;
            let apply = apply_collection_callback(ctx, &arg(&args, 0), &method(name))?;
            let ctx = ctx.clone();
            Ok(NOut::Work(boxed(async move {
                let mut counter = 0usize;
                loop {
                    let step = source.next_step(&ctx).await?;
                    if step.done {
                        return Ok(match name {
                            "forEach" | "find" => Value::Undefined,
                            "some" => Value::Bool(false),
                            _ => Value::Bool(true),
                        });
                    }
                    let result = apply(vec![step.value.clone(), Value::Num(counter as f64)], Value::Undefined).await;
                    counter += 1;
                    let result = guarded(&ctx, &source, result).await?.truthy();
                    match name {
                        "some" if result => {
                            source.close_now(&ctx).await?;
                            return Ok(Value::Bool(true));
                        }
                        "every" if !result => {
                            source.close_now(&ctx).await?;
                            return Ok(Value::Bool(false));
                        }
                        "find" if result => {
                            source.close_now(&ctx).await?;
                            return Ok(step.value);
                        }
                        _ => {}
                    }
                }
            })))
        });
    }
    // Helpers and `Iterator.from` wrappers can be closed; collection iterators, as in JS, cannot.
    ctx.method_work(&ctx.builtins.iterator_helper, "return", 0, |ctx, this, _, _| {
        let target = receiver(&this, |obj| matches!(obj.kind(), Kind::Iterator(_)), "Iterator.prototype.return")?;
        let Kind::Iterator(cursor) = target.kind() else { unreachable!() };
        let cursor = cursor.clone();
        let ctx = ctx.clone();
        Ok(NOut::Work(boxed(async move {
            cursor.close_now(&ctx).await?;
            Ok(Value::Obj(ctx.iter_result(Value::Undefined, true)))
        })))
    });
    let iterator = ctx.constructor(
        &proto,
        NativeOptions {
            name: "Iterator".into(),
            length: 0,
            call: requires_new("Iterator"),
            construct: Some(Rc::new(|_, _, _, _| Err(type_error("Iterator is abstract; use Iterator.from(...) or a built-in iterator.")))),
            callback: true,
        },
    );
    let from = ctx.native_work("from", 1, |ctx, _, args, _| {
        let value = arg(&args, 0);
        if let Value::Obj(_) = &value
            && has_prototype(&value, &ctx.builtins.iterator)
        {
            return Ok(NOut::Val(value));
        }
        let ctx = ctx.clone();
        Ok(NOut::Work(boxed(async move {
            let cursor = flattenable(&ctx, &value, "Iterator.from", true).await?;
            Ok(Value::Obj(ctx.new_object(Some(ctx.builtins.iterator_helper.clone()), Kind::Iterator(cursor))))
        })))
    });
    define(&iterator, PropKey::from_str("from"), Value::Obj(from), HIDDEN);
    Value::Obj(iterator)
}
