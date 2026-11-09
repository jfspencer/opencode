//! The global coercion functions (value.ts): `Number`, `String`, `Boolean`, `parseInt`, `parseFloat`, `isFinite`,
//! `isNaN`, one function per runtime and name.

use crate::interp::callback::{Hint, with_primitives};
use crate::interp::coerce::{to_number_checked, to_string_plain};
use crate::interp::value::*;
use crate::interp::{Ctx, R};
use crate::numconv;

pub fn coerce(ctx: &Ctx, name: &'static str, args: Vec<Value>) -> R<NOut> {
    // Native: Number() is 0 and String() is "", unlike their undefined-argument forms.
    if args.is_empty() {
        if name == "Number" {
            return Ok(NOut::Val(Value::Num(0.0)));
        }
        if name == "String" {
            return Ok(NOut::Val(Value::str("")));
        }
    }
    let raw = args.first().cloned().unwrap_or_default();
    if name == "Boolean" {
        return Ok(NOut::Val(Value::Bool(raw.truthy())));
    }
    if name == "parseInt" {
        let radix = args.get(1).cloned().unwrap_or_default();
        return with_primitives(ctx, &[Hint::String, Hint::Number], vec![raw, radix], |_, values| {
            let text = to_string_plain(&values[0]);
            let radix = to_number_checked(&values[1])?;
            Ok(NOut::Val(Value::Num(numconv::parse_int(text.units(), radix))))
        });
    }
    let hint = if name == "String" || name == "parseFloat" { Hint::String } else { Hint::Number };
    with_primitives(ctx, &[hint], vec![raw], move |_, values| {
        let value = &values[0];
        Ok(NOut::Val(match name {
            "Number" => Value::Num(to_number_checked(value)?),
            "isFinite" => Value::Bool(to_number_checked(value)?.is_finite()),
            "isNaN" => Value::Bool(to_number_checked(value)?.is_nan()),
            "parseFloat" => Value::Num(numconv::parse_float(to_string_plain(value).units())),
            _ => Value::Str(to_string_plain(value)),
        }))
    })
}

/// A global coercion function such as `Number` or `parseInt`.
pub fn coercion(ctx: &Ctx, name: &'static str, length: u32) -> Obj {
    if let Some(known) = ctx.coercions.borrow().get(name) {
        return known.clone();
    }
    let created = ctx.native_work(name, length, move |ctx, _, args, _| coerce(ctx, name, args));
    ctx.coercions.borrow_mut().insert(name, created.clone());
    created
}
