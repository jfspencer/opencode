//! One program execution (interpreter.ts `Interpreter`): the built-in prototypes, the job queue, captured logs, the
//! tool bridge, and the global scope built once.

use crate::interp::coerce::describe_value;
use crate::interp::cursor::{Cursor, HostIter, IterKind, require_iterator_method, require_iterator_object};
use crate::interp::errors::{ErrorType, Fail, PendingThrow, type_error};
use crate::interp::object::{define, entries, get};
use crate::interp::runner::{AwaitLeaf, Runner, Start, await_leaf};
use crate::interp::scope::{Binding, Scope};
use crate::interp::value::*;
use crate::interp::{Ctx, Fut, Loc, R, boxed};
use crate::jsstr::JsStr;
use crate::tools::ToolSession;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub struct Builtins {
    pub object: Obj,
    pub function: Obj,
    pub array: Obj,
    pub string: Obj,
    pub number: Obj,
    pub boolean: Obj,
    pub date: Obj,
    pub regexp: Obj,
    pub map: Obj,
    pub set: Obj,
    pub weak_map: Obj,
    pub weak_set: Obj,
    pub url: Obj,
    pub url_search_params: Obj,
    pub headers: Obj,
    pub uint8_array: Obj,
    pub text_encoder: Obj,
    pub text_decoder: Obj,
    pub promise: Obj,
    pub iterator: Obj,
    pub iterator_helper: Obj,
    pub async_iterator: Obj,
    pub generator: Obj,
    pub async_generator: Obj,
    errors: Vec<Obj>,
}

impl Builtins {
    pub fn error(&self, ty: ErrorType) -> &Obj {
        let index = crate::interp::errors::ERROR_TYPES.iter().position(|t| *t == ty).unwrap();
        &self.errors[index]
    }
}

/// A program iterator as `for...of` drives it: the object, its `next`, and whether it is asynchronous.
#[derive(Clone)]
pub struct CustomIterator {
    pub iterator: Obj,
    pub next: Value,
    pub asynchronous: bool,
}

pub struct Interp {
    pub builtins: Builtins,
    pub runner: Runner,
    pub registry: Registry,
    pub tools: Rc<ToolSession>,
    pub logs: Rc<RefCell<Vec<JsStr>>>,
    /// Template objects by site.
    pub templates: RefCell<HashMap<usize, Obj>>,
    pub global_scope: Scope,
    modules: RefCell<HashMap<JsStr, Value>>,
    namespaces: RefCell<HashMap<JsStr, Obj>>,
    /// One function per coercion name, so `Number.parseInt === parseInt`.
    pub coercions: RefCell<HashMap<&'static str, Obj>>,
    pub promise_like: RefCell<Option<Obj>>,
    /// The program's result once it has crossed the boundary.
    pub main_result: RefCell<Option<crate::json::Json>>,
    /// Whether each function (by its node) reads `arguments`, found once as TS memoizes it.
    arguments_use: RefCell<HashMap<usize, bool>>,
}

fn raw_object(registry: &Registry, proto: Option<Obj>, kind: Kind) -> Obj {
    let obj = Obj(Rc::new(ObjInner {
        proto: RefCell::new(proto),
        props: RefCell::new(Default::default()),
        extensible: std::cell::Cell::new(true),
        kind,
    }));
    registry.add(&obj);
    obj
}

fn create_builtins(registry: &Registry) -> Builtins {
    let object = raw_object(registry, None, Kind::Ordinary);
    // Function.prototype is itself callable and returns undefined.
    let function = raw_object(
        registry,
        Some(object.clone()),
        Kind::Function(Callable::Native(NativeFn {
            call: RefCell::new(Rc::new(|_, _, _, _| Ok(NOut::Val(Value::Undefined)))),
            construct: RefCell::new(None),
            callback: true,
        })),
    );
    define(&function, PropKey::from_str("length"), Value::Num(0.0), READONLY);
    define(&function, PropKey::from_str("name"), Value::str(""), READONLY);
    let plain = || raw_object(registry, Some(object.clone()), Kind::Ordinary);
    let error = plain();
    define(&error, PropKey::from_str("name"), Value::str("Error"), HIDDEN);
    define(&error, PropKey::from_str("message"), Value::str(""), HIDDEN);
    let mut errors = vec![error.clone()];
    for ty in &crate::interp::errors::ERROR_TYPES[1..] {
        let proto = raw_object(registry, Some(error.clone()), Kind::Ordinary);
        define(&proto, PropKey::from_str("name"), Value::str(ty.name()), HIDDEN);
        define(&proto, PropKey::from_str("message"), Value::str(""), HIDDEN);
        errors.push(proto);
    }
    let iterator = plain();
    let async_iterator = plain();
    Builtins {
        array: raw_object(registry, Some(object.clone()), Kind::Array(ArrData::new(Vec::new()))),
        string: plain(),
        number: plain(),
        boolean: plain(),
        date: plain(),
        regexp: plain(),
        map: plain(),
        set: plain(),
        weak_map: plain(),
        weak_set: plain(),
        url: plain(),
        url_search_params: plain(),
        headers: plain(),
        uint8_array: plain(),
        text_encoder: plain(),
        text_decoder: plain(),
        promise: plain(),
        iterator_helper: raw_object(registry, Some(iterator.clone()), Kind::Ordinary),
        generator: raw_object(registry, Some(iterator.clone()), Kind::Ordinary),
        async_generator: raw_object(registry, Some(async_iterator.clone()), Kind::Ordinary),
        iterator,
        async_iterator,
        errors,
        function,
        object,
    }
}

impl Interp {
    pub fn new(
        tools: Rc<ToolSession>,
        logs: Rc<RefCell<Vec<JsStr>>>,
        globals: impl FnOnce(&Ctx) -> Vec<(JsStr, Value)>,
    ) -> Ctx {
        let registry = Registry::default();
        let builtins = create_builtins(&registry);
        let ctx = Rc::new(Interp {
            builtins,
            runner: Runner::default(),
            registry,
            tools,
            logs,
            templates: Default::default(),
            global_scope: Scope::new(),
            modules: Default::default(),
            namespaces: Default::default(),
            coercions: Default::default(),
            promise_like: Default::default(),
            main_result: Default::default(),
            arguments_use: Default::default(),
        });
        // Program code has no receiver: top-level `this` is undefined, as in a module.
        ctx.global_scope.set_binding(Rc::from("this"), Binding { mutable: false, value: Value::Undefined, initialized: true });
        let mut all = crate::stdlib::globals(&ctx);
        all.extend(globals(&ctx));
        for (name, value) in all {
            let name: Rc<str> = Rc::from(name.to_rust_lossy().as_str());
            ctx.global_scope.set_binding(name, Binding { mutable: false, value, initialized: true });
        }
        ctx
    }

    /// Whether a function's parameters or body read `arguments` (memoized; a walk that overflows is retried, as in TS).
    pub fn uses_arguments(&self, node: &Rc<crate::parser::ast::Function>) -> crate::interp::R<bool> {
        let key = Rc::as_ptr(node) as usize;
        if let Some(found) = self.arguments_use.borrow().get(&key) {
            return Ok(*found);
        }
        let found = crate::interp::walk::uses_arguments(node)?;
        self.arguments_use.borrow_mut().insert(key, found);
        Ok(found)
    }

    /// Breaks the run's object cycles so its memory is freed.
    pub fn dispose(&self) {
        self.runner.clear();
        self.templates.borrow_mut().clear();
        self.modules.borrow_mut().clear();
        self.namespaces.borrow_mut().clear();
        self.coercions.borrow_mut().clear();
        self.promise_like.borrow_mut().take();
        self.global_scope.0.borrow_mut().clear();
        self.registry.break_cycles();
    }

    // --- Object creation ---

    pub fn new_object(&self, proto: Option<Obj>, kind: Kind) -> Obj {
        raw_object(&self.registry, proto, kind)
    }

    pub fn new_plain(&self) -> Obj {
        self.new_object(Some(self.builtins.object.clone()), Kind::Ordinary)
    }

    pub fn new_array(&self, items: Vec<Value>) -> Obj {
        self.new_array_holes(items.into_iter().map(Some).collect())
    }

    pub fn new_array_holes(&self, items: Vec<Option<Value>>) -> Obj {
        self.new_object(Some(self.builtins.array.clone()), Kind::Array(ArrData::new(items)))
    }

    pub fn new_array_with_proto(&self, proto: Obj, items: Vec<Option<Value>>) -> Obj {
        self.new_object(Some(proto), Kind::Array(ArrData::new(items)))
    }

    pub fn new_function(&self, callable: Callable, name: &str, length: u32) -> Obj {
        self.new_function_js(callable, JsStr::from(name), length)
    }

    pub fn new_function_js(&self, callable: Callable, name: JsStr, length: u32) -> Obj {
        let function = self.new_object(Some(self.builtins.function.clone()), Kind::Function(callable));
        define(&function, PropKey::from_str("length"), Value::Num(length as f64), READONLY);
        define(&function, PropKey::from_str("name"), Value::Str(name), READONLY);
        function
    }

    pub fn create_error(&self, ty: ErrorType, message: Option<JsStr>) -> Obj {
        self.create_error_with(self.builtins.error(ty).clone(), message)
    }

    pub fn create_error_with(&self, proto: Obj, message: Option<JsStr>) -> Obj {
        let error = self.new_object(Some(proto), Kind::Error(RefCell::new(None)));
        if let Some(message) = message {
            define(&error, PropKey::from_str("message"), Value::Str(message), HIDDEN);
        }
        error
    }

    pub fn new_promise_object(&self, id: u64) -> Obj {
        self.new_object(
            Some(self.builtins.promise.clone()),
            Kind::Promise(PromiseData {
                state: std::cell::Cell::new(PromiseState::Pending),
                result: RefCell::new(None),
                reactions: RefCell::new(Vec::new()),
                handled: std::cell::Cell::new(false),
                id,
            }),
        )
    }

    /// An iterator result `{ value, done }`.
    pub fn iter_result(&self, value: Value, done: bool) -> Obj {
        let target = self.new_plain();
        define(&target, PropKey::from_str("value"), value, DATA);
        define(&target, PropKey::from_str("done"), Value::Bool(done), DATA);
        target
    }

    pub fn record(&self, fields: Vec<(&str, Value)>) -> Obj {
        let target = self.new_plain();
        for (key, value) in fields {
            define(&target, PropKey::from_str(key), value, DATA);
        }
        target
    }

    /// The strict `arguments` object: indexed own properties and a hidden `length`.
    pub fn new_arguments(&self, args: &[Value]) -> Obj {
        let target = self.new_object(Some(self.builtins.object.clone()), Kind::Arguments);
        for (index, arg) in args.iter().enumerate() {
            define(&target, PropKey::Index(index as u32), arg.clone(), DATA);
        }
        define(&target, PropKey::from_str("length"), Value::Num(args.len() as f64), HIDDEN);
        target
    }

    /// The prototype a primitive reads its methods from; none for null, undefined, and symbols.
    pub fn primitive_prototype(&self, value: &Value) -> Option<Obj> {
        match value {
            Value::Str(_) => Some(self.builtins.string.clone()),
            Value::Num(_) => Some(self.builtins.number.clone()),
            Value::Bool(_) => Some(self.builtins.boolean.clone()),
            _ => None,
        }
    }

    // --- Calling and awaiting ---

    /// Calls into the program; a built-in passes its own call site so failures inside are located there.
    pub fn call(self: &Rc<Self>, callable: Value, this: Value, args: Vec<Value>, site: Option<Loc>) -> Fut<R<Value>> {
        match crate::interp::expr::dispatch(self, callable, this, args, site, None) {
            Ok(work) => work,
            Err(error) => boxed(async move { Err(error) }),
        }
    }

    /// Await: suspends the innermost async body until `value` settles.
    pub fn await_value(self: &Rc<Self>, value: Value, loc: Option<Loc>) -> AwaitLeaf {
        await_leaf(self, value, loc)
    }

    pub fn start_task(self: &Rc<Self>, body: impl std::future::Future<Output = R<Value>> + 'static) -> Start {
        Start::new(crate::interp::runner::activation(self, boxed(body)))
    }

    // --- Iteration ---

    pub fn builtin_cursor(&self, value: &Value) -> Option<Rc<Cursor>> {
        let iter = match value {
            Value::Str(text) => HostIter::String { text: text.clone(), index: 0 },
            Value::Obj(obj) => match obj.kind() {
                Kind::Iterator(cursor) => return Some(cursor.clone()),
                Kind::Array(_) => HostIter::Array { target: obj.clone(), index: 0, kind: IterKind::Values },
                Kind::Map(_) => HostIter::Map { target: obj.clone(), position: 0, kind: IterKind::Entries },
                Kind::Set(_) => HostIter::Set { target: obj.clone(), position: 0, kind: IterKind::Values },
                Kind::Bytes(_) => HostIter::Bytes { target: obj.clone(), index: 0, kind: IterKind::Values },
                Kind::URLSearchParams(_) => HostIter::SearchParams { target: obj.clone(), index: 0, kind: IterKind::Entries },
                Kind::Headers(_) => HostIter::Headers { target: obj.clone(), index: 0, kind: IterKind::Entries },
                Kind::Arguments => {
                    let items = crate::interp::object::keys(obj)
                        .iter()
                        .map(|key| crate::interp::object::get_data(obj, key))
                        .collect();
                    HostIter::List { items, index: 0 }
                }
                _ => return None,
            },
            _ => return None,
        };
        Some(Cursor::host(iter))
    }

    pub async fn custom_iterator(self: &Rc<Self>, value: &Value, loc: Option<Loc>, allow_async: bool) -> R<Option<CustomIterator>> {
        let Value::Obj(obj) = value else { return Ok(None) };
        let async_method =
            if allow_async { get(self, obj, &PropKey::Sym(Sym::AsyncIterator))? } else { Value::Undefined };
        let method = if async_method.is_undefined() { get(self, obj, &PropKey::Sym(Sym::Iterator))? } else { async_method.clone() };
        if method.is_nullish() {
            return Ok(None);
        }
        let method = require_iterator_method(method, "Iterator method", loc)?;
        let iterator = self.call(method, value.clone(), Vec::new(), loc).await?;
        let object = require_iterator_object(iterator, "Iterator method result", loc)?;
        let next = require_iterator_method(get(self, &object, &PropKey::from_str("next"))?, "Iterator next", loc)?;
        Ok(Some(CustomIterator { iterator: object, next, asynchronous: !async_method.is_nullish() }))
    }

    pub async fn iterate(self: &Rc<Self>, value: &Value, loc: Option<Loc>) -> R<Option<Rc<Cursor>>> {
        if let Some(cursor) = self.builtin_cursor(value) {
            return Ok(Some(cursor));
        }
        Ok(self
            .custom_iterator(value, loc, false)
            .await?
            .map(|custom| Cursor::custom(custom.iterator, custom.next, false, loc)))
    }

    /// GetIteratorDirect: drive an iterator by its own `next`, without asking for `[Symbol.iterator]`.
    pub fn iterate_direct(self: &Rc<Self>, value: &Value, loc: Option<Loc>) -> R<Rc<Cursor>> {
        if let Value::Obj(obj) = value
            && let Kind::Iterator(cursor) = obj.kind()
        {
            return Ok(cursor.clone());
        }
        let Value::Obj(obj) = value else {
            return Err(type_error(format!("An iterator must be an object, received {}.", describe_value(value))).at(loc));
        };
        let next = require_iterator_method(get(self, obj, &PropKey::from_str("next"))?, "Iterator next", loc)?;
        Ok(Cursor::custom(obj.clone(), next, false, loc))
    }

    // --- Modules ---

    fn global(&self, name: &str) -> Value {
        self.global_scope.0.borrow().get(name).map(|binding| binding.value.clone()).unwrap_or_default()
    }

    /// A module, as `require` returns it and a default import binds it: a global, by its Node module name.
    pub fn module(&self, specifier: &JsStr, site: Option<Loc>) -> R<Value> {
        let name = match specifier.to_rust_lossy().strip_prefix("node:") {
            Some(rest) => JsStr::from(rest),
            None => specifier.clone(),
        };
        if let Some(cached) = self.modules.borrow().get(&name) {
            return Ok(cached.clone());
        }
        let text = name.to_rust_lossy();
        let mut module = self.global(&text);
        if module.is_undefined() {
            module = match text.as_str() {
                "fs/promises" => match self.global("fs") {
                    Value::Obj(fs) => crate::interp::object::get_data(&fs, &PropKey::from_str("promises")),
                    _ => Value::Undefined,
                },
                "url" => self.pick(&["URL", "URLSearchParams"]),
                "util" => self.pick(&["TextEncoder", "TextDecoder"]),
                "buffer" => self.pick(&["Buffer", "atob", "btoa"]),
                _ => Value::Undefined,
            };
        }
        if module.is_undefined() {
            return Err(Fail::Pending(Rc::new(PendingThrow {
                ty: ErrorType::Error,
                message: JsStr::from(format!(
                    "Cannot import \"{specifier}\": it isn't available in Code Mode, which can't load packages or files. Use the provided globals, or call tools through `tools`."
                )),
                loc: std::cell::Cell::new(site),
                kind: crate::interp::errors::DiagnosticKind::ExecutionFailure,
                suggestions: None,
                value: RefCell::new(None),
                name: None,
            })));
        }
        self.modules.borrow_mut().insert(name, module.clone());
        Ok(module)
    }

    fn pick(&self, names: &[&str]) -> Value {
        let found: Vec<(&str, Value)> =
            names.iter().filter_map(|name| Some((*name, self.global(name))).filter(|(_, v)| !v.is_undefined())).collect();
        if found.is_empty() {
            return Value::Undefined;
        }
        let module = self.new_object(None, Kind::Ordinary);
        for (name, value) in found {
            define(&module, PropKey::from_str(name), value, DATA);
        }
        Value::Obj(module)
    }

    /// What `import * as` and `import()` see: the module's own properties, and the module itself as `default`.
    pub fn namespace(self: &Rc<Self>, specifier: &JsStr, site: Option<Loc>) -> R<Obj> {
        let module = self.module(specifier, site)?;
        let name = match specifier.to_rust_lossy().strip_prefix("node:") {
            Some(rest) => JsStr::from(rest),
            None => specifier.clone(),
        };
        if let Some(cached) = self.namespaces.borrow().get(&name) {
            return Ok(cached.clone());
        }
        let namespace = self.new_object(None, Kind::Ordinary);
        if let Value::Obj(obj) = &module {
            for (key, value) in entries(self, obj)? {
                define(&namespace, key, value, DATA);
            }
        }
        define(&namespace, PropKey::from_str("default"), module, DATA);
        self.namespaces.borrow_mut().insert(name, namespace.clone());
        Ok(namespace)
    }
}

// CreateAsyncFromSyncIterator: an async iterator over a synchronous one. Each `next()` promise settles a job after
// the value does, and a rejected value closes the synchronous iterator first; `return()` calls the synchronous
// iterator's own `return`, or settles at once when it has none.
pub fn async_from_sync(
    ctx: &Ctx,
    builtin: Option<Rc<Cursor>>,
    custom: Option<CustomIterator>,
    loc: Option<Loc>,
) -> CustomIterator {
    use crate::interp::promises::capability;
    struct Adapter {
        builtin: Option<Rc<Cursor>>,
        custom: Option<CustomIterator>,
        loc: Option<Loc>,
    }
    let adapter_state = Rc::new(Adapter { builtin, custom, loc });

    fn settle(ctx: &Ctx, state: &Rc<Adapter>, step: crate::interp::cursor::Step, close_on_rejection: bool, made: crate::interp::runner::Resolving) {
        let rejected = {
            let state = state.clone();
            let done = step.done;
            let made = made.clone();
            move |ctx: &Ctx, reason: Fail| -> Option<Start> {
                if done || !close_on_rejection || reason.is_interrupt() {
                    made.reject(ctx, reason);
                    return None;
                }
                let (ctx2, state) = (ctx.clone(), state.clone());
                Some(ctx.start_task(async move {
                    // The rejection wins over a failing close.
                    let _ = match &state.custom {
                        Some(custom) => {
                            crate::interp::cursor::close_iterator(&ctx2, &custom.iterator, custom.asynchronous, state.loc).await
                        }
                        None => state.builtin.as_ref().unwrap().close_now(&ctx2).await,
                    };
                    made.reject(&ctx2, reason);
                    Ok(Value::Undefined)
                }))
            }
        };
        match ctx.runner.promise_resolve(ctx, step.value) {
            Err(error) => {
                if let Some(start) = rejected(ctx, error) {
                    ctx.runner.enqueue(move |_| Some(start));
                }
            }
            Ok(wrapper) => {
                let done = step.done;
                ctx.runner.then_with(&wrapper, move |ctx, settled| match settled {
                    Ok(value) => {
                        made.resolve(ctx, Value::Obj(ctx.iter_result(value, done)));
                        None
                    }
                    Err(error) => rejected(ctx, error),
                });
            }
        }
    }

    let next = {
        let state = adapter_state.clone();
        ctx.native_work("next", 1, move |ctx, _, args, _| {
            let (ctx, state) = (ctx.clone(), state.clone());
            Ok(NOut::Work(boxed(async move {
                let made = capability(&ctx, state.loc)?;
                let step = async {
                    match &state.custom {
                        None => state.builtin.as_ref().unwrap().next_step(&ctx).await,
                        Some(custom) => {
                            let called = ctx.call(custom.next.clone(), Value::Obj(custom.iterator.clone()), args, state.loc).await?;
                            let object = require_iterator_object(called, "Iterator next() result", state.loc)?;
                            Ok(crate::interp::cursor::Step {
                                done: get(&ctx, &object, &PropKey::from_str("done"))?.truthy(),
                                value: get(&ctx, &object, &PropKey::from_str("value"))?,
                            })
                        }
                    }
                }
                .await;
                match step {
                    Ok(step) => settle(&ctx, &state, step, true, made.clone()),
                    Err(error) => made.reject(&ctx, error),
                }
                Ok(Value::Obj(made.promise().clone()))
            })))
        })
    };
    let close = {
        let state = adapter_state.clone();
        ctx.native_work("return", 1, move |ctx, _, args, _| {
            let (ctx, state) = (ctx.clone(), state.clone());
            Ok(NOut::Work(boxed(async move {
                let made = capability(&ctx, state.loc)?;
                let outcome: R<()> = async {
                    let method = match &state.custom {
                        Some(custom) => get(&ctx, &custom.iterator, &PropKey::from_str("return"))?,
                        None => Value::Undefined,
                    };
                    if method.is_nullish() {
                        if let Some(builtin) = &state.builtin {
                            builtin.close_now(&ctx).await?;
                        }
                        made.resolve(&ctx, Value::Obj(ctx.iter_result(args.first().cloned().unwrap_or_default(), true)));
                        return Ok(());
                    }
                    let custom = state.custom.as_ref().unwrap();
                    let method = require_iterator_method(method, "Iterator return", state.loc)?;
                    let returned = ctx.call(method, Value::Obj(custom.iterator.clone()), args, state.loc).await?;
                    let returned = require_iterator_object(returned, "Iterator return() result", state.loc)?;
                    let step = crate::interp::cursor::Step {
                        done: get(&ctx, &returned, &PropKey::from_str("done"))?.truthy(),
                        value: get(&ctx, &returned, &PropKey::from_str("value"))?,
                    };
                    settle(&ctx, &state, step, false, made.clone());
                    Ok(())
                }
                .await;
                if let Err(error) = outcome {
                    made.reject(&ctx, error);
                }
                Ok(Value::Obj(made.promise().clone()))
            })))
        })
    };
    let raise = {
        let state = adapter_state.clone();
        ctx.native_work("throw", 1, move |ctx, _, args, _| {
            let (ctx, state) = (ctx.clone(), state.clone());
            Ok(NOut::Work(boxed(async move {
                let made = capability(&ctx, state.loc)?;
                let outcome: R<()> = async {
                    let method = match &state.custom {
                        Some(custom) => get(&ctx, &custom.iterator, &PropKey::from_str("throw"))?,
                        None => Value::Undefined,
                    };
                    if method.is_nullish() {
                        match &state.custom {
                            Some(custom) => {
                                crate::interp::cursor::close_iterator(&ctx, &custom.iterator, custom.asynchronous, state.loc).await?
                            }
                            None => state.builtin.as_ref().unwrap().close_now(&ctx).await?,
                        }
                        made.reject(&ctx, type_error("The delegated iterator does not provide a throw() method.").at(state.loc));
                        return Ok(());
                    }
                    let custom = state.custom.as_ref().unwrap();
                    let method = require_iterator_method(method, "Iterator throw", state.loc)?;
                    let thrown = ctx.call(method, Value::Obj(custom.iterator.clone()), args, state.loc).await?;
                    let thrown = require_iterator_object(thrown, "Iterator throw() result", state.loc)?;
                    let step = crate::interp::cursor::Step {
                        done: get(&ctx, &thrown, &PropKey::from_str("done"))?.truthy(),
                        value: get(&ctx, &thrown, &PropKey::from_str("value"))?,
                    };
                    settle(&ctx, &state, step, true, made.clone());
                    Ok(())
                }
                .await;
                if let Err(error) = outcome {
                    made.reject(&ctx, error);
                }
                Ok(Value::Obj(made.promise().clone()))
            })))
        })
    };
    let adapter = ctx.new_plain();
    define(&adapter, PropKey::from_str("next"), Value::Obj(next.clone()), HIDDEN);
    define(&adapter, PropKey::from_str("return"), Value::Obj(close), HIDDEN);
    define(&adapter, PropKey::from_str("throw"), Value::Obj(raise), HIDDEN);
    CustomIterator { iterator: adapter, next: Value::Obj(next), asynchronous: true }
}
