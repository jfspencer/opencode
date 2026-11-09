//! How the interpreter drives any iterator (objects.ts `Cursor`): pull the next step, or close it early. Built-in
//! collections iterate live and synchronously; program iterators and iterator helpers run program code.

use crate::interp::errors::type_error;
use crate::interp::object::get;
use crate::interp::value::*;
use crate::interp::{Ctx, Fut, Loc, R, boxed};
use crate::jsstr::JsStr;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone)]
pub struct Step {
    pub done: bool,
    pub value: Value,
}

impl Step {
    pub fn done() -> Step {
        Step { done: true, value: Value::Undefined }
    }
    pub fn of(value: Value) -> Step {
        Step { done: false, value }
    }
}

/// A result available now, or work that runs program code first.
pub enum Pull<T> {
    Ready(R<T>),
    Work(Fut<R<T>>),
}

impl<T: 'static> Pull<T> {
    pub async fn get(self) -> R<T> {
        match self {
            Pull::Ready(result) => result,
            Pull::Work(work) => work.await,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IterKind {
    Keys,
    Values,
    Entries,
}

/// A live cursor over a built-in collection.
pub enum HostIter {
    Array { target: Obj, index: usize, kind: IterKind },
    String { text: JsStr, index: usize },
    Map { target: Obj, position: usize, kind: IterKind },
    Set { target: Obj, position: usize, kind: IterKind },
    Bytes { target: Obj, index: usize, kind: IterKind },
    List { items: Vec<Value>, index: usize },
    SearchParams { target: Obj, index: usize, kind: IterKind },
    Headers { target: Obj, index: usize, kind: IterKind },
    Done,
}

/// Iterator helpers and other cursors implemented in Rust that run program code.
pub trait DynCursor {
    fn next(self: Rc<Self>, ctx: &Ctx) -> Pull<Step>;
    fn close(self: Rc<Self>, ctx: &Ctx) -> Pull<()>;
    fn clear(&self) {}
}

pub enum Cursor {
    Host(RefCell<HostIter>),
    /// A program iterator object driven by its own `next` (customCursor).
    Custom { iterator: RefCell<Option<Obj>>, next: RefCell<Value>, asynchronous: bool, loc: Option<Loc> },
    Dyn(RefCell<Option<Rc<dyn DynCursor>>>),
    /// A cursor whose state was cleared at the end of a run.
    Cleared(Cell<()>),
}

impl Cursor {
    pub fn host(iter: HostIter) -> Rc<Cursor> {
        Rc::new(Cursor::Host(RefCell::new(iter)))
    }
    pub fn dynamic(cursor: Rc<dyn DynCursor>) -> Rc<Cursor> {
        Rc::new(Cursor::Dyn(RefCell::new(Some(cursor))))
    }
    pub fn custom(iterator: Obj, next: Value, asynchronous: bool, loc: Option<Loc>) -> Rc<Cursor> {
        Rc::new(Cursor::Custom { iterator: RefCell::new(Some(iterator)), next: RefCell::new(next), asynchronous, loc })
    }

    pub fn clear(&self) {
        match self {
            Cursor::Host(iter) => *iter.borrow_mut() = HostIter::Done,
            Cursor::Custom { iterator, next, .. } => {
                iterator.borrow_mut().take();
                *next.borrow_mut() = Value::Undefined;
            }
            Cursor::Dyn(inner) => {
                if let Some(cursor) = inner.borrow_mut().take() {
                    cursor.clear();
                }
            }
            Cursor::Cleared(_) => {}
        }
    }

    pub fn next(self: &Rc<Self>, ctx: &Ctx) -> Pull<Step> {
        match &**self {
            Cursor::Host(iter) => Pull::Ready(Ok(host_next(ctx, &mut iter.borrow_mut()))),
            Cursor::Custom { iterator, next, asynchronous, loc } => {
                let Some(iterator) = iterator.borrow().clone() else { return Pull::Ready(Ok(Step::done())) };
                let next = next.borrow().clone();
                let (ctx, asynchronous, loc) = (ctx.clone(), *asynchronous, *loc);
                Pull::Work(boxed(async move { next_iterator_result(&ctx, &iterator, &next, asynchronous, loc).await }))
            }
            Cursor::Dyn(inner) => match inner.borrow().clone() {
                Some(cursor) => cursor.next(ctx),
                None => Pull::Ready(Ok(Step::done())),
            },
            Cursor::Cleared(_) => Pull::Ready(Ok(Step::done())),
        }
    }

    pub fn close(self: &Rc<Self>, ctx: &Ctx) -> Pull<()> {
        match &**self {
            Cursor::Host(_) | Cursor::Cleared(_) => Pull::Ready(Ok(())),
            Cursor::Custom { iterator, asynchronous, loc, .. } => {
                let Some(iterator) = iterator.borrow().clone() else { return Pull::Ready(Ok(())) };
                let (ctx, asynchronous, loc) = (ctx.clone(), *asynchronous, *loc);
                Pull::Work(boxed(async move { close_iterator(&ctx, &iterator, asynchronous, loc).await }))
            }
            Cursor::Dyn(inner) => match inner.borrow().clone() {
                Some(cursor) => cursor.close(ctx),
                None => Pull::Ready(Ok(())),
            },
        }
    }

    pub async fn next_step(self: &Rc<Self>, ctx: &Ctx) -> R<Step> {
        self.next(ctx).get().await
    }

    pub async fn close_now(self: &Rc<Self>, ctx: &Ctx) -> R<()> {
        self.close(ctx).get().await
    }
}

fn entry(ctx: &Ctx, key: Value, value: Value) -> Value {
    Value::Obj(ctx.new_array(vec![key, value]))
}

fn host_next(ctx: &Ctx, iter: &mut HostIter) -> Step {
    match iter {
        HostIter::Array { target, index, kind } => {
            let Some(data) = target.array() else { return Step::done() };
            if *index >= data.len() {
                *iter = HostIter::Done;
                return Step::done();
            }
            let at = *index;
            *index += 1;
            let item = data.at(at);
            match kind {
                IterKind::Keys => Step::of(Value::Num(at as f64)),
                IterKind::Values => Step::of(item),
                IterKind::Entries => Step::of(entry(ctx, Value::Num(at as f64), item)),
            }
        }
        HostIter::String { text, index } => {
            let units = text.units();
            if *index >= units.len() {
                *iter = HostIter::Done;
                return Step::done();
            }
            let start = *index;
            let first = units[start];
            let width = if (0xD800..0xDC00).contains(&first)
                && units.get(start + 1).is_some_and(|second| (0xDC00..0xE000).contains(second))
            {
                2
            } else {
                1
            };
            *index += width;
            Step::of(Value::Str(JsStr::from_slice(&units[start..start + width])))
        }
        HostIter::Map { target, position, kind } => {
            let Kind::Map(map) = target.kind() else { return Step::done() };
            let next = map.borrow().entry_from(*position);
            match next {
                None => {
                    *iter = HostIter::Done;
                    Step::done()
                }
                Some((after, key, value)) => {
                    *position = after;
                    match kind {
                        IterKind::Keys => Step::of(key),
                        IterKind::Values => Step::of(value),
                        IterKind::Entries => Step::of(entry(ctx, key, value)),
                    }
                }
            }
        }
        HostIter::Set { target, position, kind } => {
            let Kind::Set(set) = target.kind() else { return Step::done() };
            let next = set.borrow().value_from(*position);
            match next {
                None => {
                    *iter = HostIter::Done;
                    Step::done()
                }
                Some((after, value)) => {
                    *position = after;
                    match kind {
                        IterKind::Entries => Step::of(entry(ctx, value.clone(), value)),
                        _ => Step::of(value),
                    }
                }
            }
        }
        HostIter::Bytes { target, index, kind } => {
            let Some(data) = target.bytes() else { return Step::done() };
            if *index >= data.length {
                *iter = HostIter::Done;
                return Step::done();
            }
            let at = *index;
            *index += 1;
            let byte = Value::Num(data.get(at).unwrap_or(0) as f64);
            match kind {
                IterKind::Keys => Step::of(Value::Num(at as f64)),
                IterKind::Values => Step::of(byte),
                IterKind::Entries => Step::of(entry(ctx, Value::Num(at as f64), byte)),
            }
        }
        HostIter::List { items, index } => {
            if *index >= items.len() {
                *iter = HostIter::Done;
                return Step::done();
            }
            *index += 1;
            Step::of(items[*index - 1].clone())
        }
        HostIter::SearchParams { target, index, kind } => {
            let Kind::URLSearchParams(params) = target.kind() else { return Step::done() };
            let pair = params.borrow().pair(*index);
            match pair {
                None => {
                    *iter = HostIter::Done;
                    Step::done()
                }
                Some((name, value)) => {
                    *index += 1;
                    match kind {
                        IterKind::Keys => Step::of(Value::Str(name)),
                        IterKind::Values => Step::of(Value::Str(value)),
                        IterKind::Entries => Step::of(entry(ctx, Value::Str(name), Value::Str(value))),
                    }
                }
            }
        }
        HostIter::Headers { target, index, kind } => {
            let Kind::Headers(headers) = target.kind() else { return Step::done() };
            let pair = headers.borrow().sorted_pair(*index);
            match pair {
                None => {
                    *iter = HostIter::Done;
                    Step::done()
                }
                Some((name, value)) => {
                    *index += 1;
                    match kind {
                        IterKind::Keys => Step::of(Value::Str(name)),
                        IterKind::Values => Step::of(Value::Str(value)),
                        IterKind::Entries => Step::of(entry(ctx, Value::Str(name), Value::Str(value))),
                    }
                }
            }
        }
        HostIter::Done => Step::done(),
    }
}

pub fn require_iterator_object(value: Value, context: &str, loc: Option<Loc>) -> R<Obj> {
    match value {
        Value::Obj(obj) => Ok(obj),
        _ => Err(type_error(format!("{context} must be an object.")).at(loc)),
    }
}

pub fn require_iterator_method(value: Value, context: &str, loc: Option<Loc>) -> R<Value> {
    if crate::interp::coerce::is_callable_value(&value) {
        return Ok(value);
    }
    Err(type_error(format!("{context} must be a function.")).at(loc))
}

pub async fn next_iterator_result(ctx: &Ctx, iterator: &Obj, next: &Value, asynchronous: bool, loc: Option<Loc>) -> R<Step> {
    let called = ctx.call(next.clone(), Value::Obj(iterator.clone()), Vec::new(), loc).await?;
    let result = if asynchronous { ctx.await_value(called, loc).await? } else { called };
    let object = require_iterator_object(result, "Iterator next() result", loc)?;
    let done = get(ctx, &object, &PropKey::from_str("done"))?.truthy();
    let value = get(ctx, &object, &PropKey::from_str("value"))?;
    Ok(Step { done, value })
}

pub async fn close_iterator(ctx: &Ctx, iterator: &Obj, asynchronous: bool, loc: Option<Loc>) -> R<()> {
    let close = get(ctx, iterator, &PropKey::from_str("return"))?;
    if close.is_nullish() {
        return Ok(());
    }
    let method = require_iterator_method(close, "Iterator return", loc)?;
    let called = ctx.call(method, Value::Obj(iterator.clone()), Vec::new(), loc).await?;
    let result = if asynchronous { ctx.await_value(called, loc).await? } else { called };
    require_iterator_object(result, "Iterator return() result", loc)?;
    Ok(())
}
