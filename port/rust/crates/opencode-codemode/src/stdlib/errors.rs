//! The Error constructors (errors.ts `errorGlobal`): callable with or without `new`, ES2022 `cause`, and
//! AggregateError's `(errors, message)` signature.

use crate::interp::callback::to_primitive_string;
use crate::interp::errors::{ErrorType, error_to_string, type_error};
use crate::interp::native::{NativeOptions, prototype_from, receiver};
use crate::interp::object::{define, get, get_data, has};
use crate::interp::value::*;
use crate::interp::{Ctx, R, boxed};
use crate::jsstr::JsStr;
use std::rc::Rc;

pub fn aggregate_error(ctx: &Ctx, errors: Vec<Value>, message: &str, proto: Option<Obj>) -> Obj {
    let proto = proto.unwrap_or_else(|| ctx.builtins.error(ErrorType::AggregateError).clone());
    let value = ctx.create_error_with(proto, Some(JsStr::from(message)));
    define(&value, PropKey::from_str("errors"), Value::Obj(ctx.new_array(errors)), HIDDEN);
    value
}

async fn construct(ctx: Ctx, ty: ErrorType, args: Vec<Value>, new_target: Obj) -> R<Value> {
    let prototype = ctx.builtins.error(ty).clone();
    let proto = prototype_from(&new_target, &prototype);
    let created = if ty == ErrorType::AggregateError {
        let message = match args.get(1) {
            None | Some(Value::Undefined) => JsStr::new(),
            Some(value) => to_primitive_string(&ctx, value.clone()).await?,
        };
        let source = args.first().cloned().unwrap_or_default();
        let Some(cursor) = ctx.iterate(&source, None).await? else {
            return Err(type_error("new AggregateError(...) expects a synchronous iterable of errors."));
        };
        let mut errors = Vec::new();
        loop {
            let step = cursor.next_step(&ctx).await?;
            if step.done {
                break;
            }
            errors.push(step.value);
        }
        let value = ctx.create_error_with(proto, Some(message));
        define(&value, PropKey::from_str("errors"), Value::Obj(ctx.new_array(errors)), HIDDEN);
        value
    } else {
        let message = match args.first() {
            None | Some(Value::Undefined) => None,
            Some(value) => Some(to_primitive_string(&ctx, value.clone()).await?),
        };
        ctx.create_error_with(proto, message)
    };
    // ES2022 `new Error(message, { cause })`: installed only when the options object has the property at all.
    let options = args.get(if ty == ErrorType::AggregateError { 2 } else { 1 });
    if let Some(Value::Obj(options)) = options
        && has(options, &PropKey::from_str("cause"))
    {
        let cause = get(&ctx, options, &PropKey::from_str("cause"))?;
        define(&created, PropKey::from_str("cause"), cause, HIDDEN);
    }
    Ok(Value::Obj(created))
}

/// An error constructor such as `Error` or `TypeError`; callable with or without `new`, like JS.
pub fn error_global(ctx: &Ctx, ty: ErrorType) -> Value {
    let prototype = ctx.builtins.error(ty).clone();
    let ctor_cell: Rc<std::cell::RefCell<Option<Obj>>> = Rc::new(std::cell::RefCell::new(None));
    let call_cell = ctor_cell.clone();
    let ctor = ctx.constructor(
        &prototype,
        NativeOptions {
            name: ty.name().into(),
            length: if ty == ErrorType::AggregateError { 2 } else { 1 },
            call: Rc::new(move |ctx, _, args, _| {
                let target = call_cell.borrow().clone().expect("constructor");
                Ok(NOut::Work(boxed(construct(ctx.clone(), ty, args, target))))
            }),
            construct: Some(Rc::new(move |ctx, args, new_target, _| {
                Ok(NOut::Work(boxed(construct(ctx.clone(), ty, args, new_target))))
            })),
            callback: true,
        },
    );
    *ctor_cell.borrow_mut() = Some(ctor.clone());
    if ty == ErrorType::Error {
        ctx.method(&prototype, "toString", 0, |ctx, this, _, _| {
            let target = receiver(&this, |_| true, "Error.prototype.toString")?;
            Ok(Value::Str(error_to_string(ctx, &target)))
        });
        ctx.method(&ctor, "isError", 1, |_, _, args, _| {
            Ok(Value::Bool(matches!(args.first(), Some(Value::Obj(obj)) if obj.is_error())))
        });
        return Value::Obj(ctor);
    }
    // Derived constructors extend Error itself, so its statics are inherited.
    if let Value::Obj(error_ctor) = get_data(ctx.builtins.error(ErrorType::Error), &PropKey::from_str("constructor")) {
        ctor.set_proto(Some(error_ctor));
    }
    Value::Obj(ctor)
}
