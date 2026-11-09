//! Building built-in functions (native.ts): natives, method tables, constructors wired to their prototypes, and
//! receiver checks.

use crate::interp::coerce::describe_value;
use crate::interp::errors::type_error;
use crate::interp::interp::Interp;
use crate::interp::object::define;
use crate::interp::value::*;
use crate::interp::{Ctx, Loc, R};
use std::rc::Rc;

pub struct NativeOptions {
    pub name: String,
    pub length: u32,
    pub call: NativeCall,
    pub construct: Option<NativeConstruct>,
    /// Whether callback sites admit this function.
    pub callback: bool,
}

impl Interp {
    pub fn new_native(&self, options: NativeOptions) -> Obj {
        let NativeOptions { name, length, call, construct, callback } = options;
        self.new_function(Callable::Native(NativeFn { call: std::cell::RefCell::new(call), construct: std::cell::RefCell::new(construct), callback }), &name, length)
    }

    /// A synchronous built-in function.
    pub fn native_fn(
        &self,
        name: &str,
        length: u32,
        body: impl Fn(&Ctx, Value, Vec<Value>, Option<Loc>) -> R<Value> + 'static,
    ) -> Obj {
        self.new_native(NativeOptions {
            name: name.into(),
            length,
            call: Rc::new(move |ctx, this, args, site| body(ctx, this, args, site).map(NOut::Val)),
            construct: None,
            callback: true,
        })
    }

    /// A built-in function that may run program code.
    pub fn native_work(
        &self,
        name: &str,
        length: u32,
        body: impl Fn(&Ctx, Value, Vec<Value>, Option<Loc>) -> R<NOut> + 'static,
    ) -> Obj {
        self.new_native(NativeOptions { name: name.into(), length, call: Rc::new(body), construct: None, callback: true })
    }

    pub fn method(
        &self,
        target: &Obj,
        name: &str,
        length: u32,
        body: impl Fn(&Ctx, Value, Vec<Value>, Option<Loc>) -> R<Value> + 'static,
    ) {
        let function = self.native_fn(name, length, body);
        define(target, PropKey::from_str(name), Value::Obj(function), HIDDEN);
    }

    pub fn method_work(
        &self,
        target: &Obj,
        name: &str,
        length: u32,
        body: impl Fn(&Ctx, Value, Vec<Value>, Option<Loc>) -> R<NOut> + 'static,
    ) {
        let function = self.native_work(name, length, body);
        define(target, PropKey::from_str(name), Value::Obj(function), HIDDEN);
    }

    pub fn constants(&self, target: &Obj, table: &[(&str, Value)]) {
        for (name, value) in table {
            define(target, PropKey::from_str(name), value.clone(), FROZEN);
        }
    }

    /// A constructor wired to its prototype: `C.prototype === proto` and `proto.constructor === C`.
    pub fn constructor(&self, proto: &Obj, options: NativeOptions) -> Obj {
        let ctor = self.new_native(options);
        define(&ctor, PropKey::from_str("prototype"), Value::Obj(proto.clone()), FROZEN);
        define(proto, PropKey::from_str("constructor"), Value::Obj(ctor.clone()), HIDDEN);
        ctor
    }
}

/// The `call` of a constructor that JS requires to be invoked with `new`.
pub fn requires_new(name: &'static str) -> NativeCall {
    Rc::new(move |_, _, _, _| Err(type_error(format!("Constructor {name} requires 'new'."))))
}

/// The instance prototype for `new` via `newTarget.prototype`, falling back to the built-in's own.
pub fn prototype_from(new_target: &Obj, fallback: &Obj) -> Obj {
    let props = new_target.0.props.borrow();
    match props.get(&PropKey::from_str("prototype")) {
        Some(Slot::Data { value: Value::Obj(proto), .. }) => proto.clone(),
        _ => fallback.clone(),
    }
}

/// Narrows a method receiver to the built-in it belongs to, or throws the TypeError JS would.
pub fn receiver(this: &Value, matches: impl Fn(&Obj) -> bool, method: &str) -> R<Obj> {
    if let Value::Obj(obj) = this
        && matches(obj)
    {
        return Ok(obj.clone());
    }
    Err(type_error(format!("{method} called on incompatible receiver {}.", describe_value(this))))
}

pub fn receiver_obj(this: &Value, method: &str) -> R<Obj> {
    receiver(this, |_| true, method)
}

pub fn receiver_generator(this: &Value, method: &str) -> R<Obj> {
    receiver(this, |obj| obj.generator().is_some(), method)
}

pub fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}
