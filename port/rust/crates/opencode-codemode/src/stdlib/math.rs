//! `Math` (math.ts).

use crate::interp::callback::{Hint, preserve_consumer_error, with_primitives};
use crate::interp::coerce::to_number_checked;
use crate::interp::errors::type_error;
use crate::interp::value::*;
use crate::interp::{Ctx, R, boxed};

// The C library's inverse hyperbolic functions, as JSC calls them (Rust's std computes these itself, and differs in
// the last place).
mod libm {
    unsafe extern "C" {
        pub fn asinh(x: f64) -> f64;
        pub fn acosh(x: f64) -> f64;
        pub fn atanh(x: f64) -> f64;
    }
}

fn c_asinh(x: f64) -> f64 {
    unsafe { libm::asinh(x) }
}

fn c_acosh(x: f64) -> f64 {
    unsafe { libm::acosh(x) }
}

fn c_atanh(x: f64) -> f64 {
    unsafe { libm::atanh(x) }
}

fn round(x: f64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    if x > 0.0 && x < 0.5 {
        return 0.0;
    }
    if x < 0.0 && x >= -0.5 {
        return -0.0;
    }
    (x + 0.5).floor()
}

fn f16round(x: f64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let a = x.abs();
    if a >= 65520.0 {
        return sign * f64::INFINITY;
    }
    // Round to the nearest half-precision value, ties to even.
    let exponent = a.log2().floor().max(-14.0);
    let quantum = 2f64.powi(exponent as i32 - 10);
    let scaled = a / quantum;
    let floor = scaled.floor();
    let diff = scaled - floor;
    let rounded = if diff > 0.5 || (diff == 0.5 && floor % 2.0 != 0.0) { floor + 1.0 } else { floor };
    sign * rounded * quantum
}

fn unary(name: &str) -> fn(f64) -> f64 {
    match name {
        "abs" => f64::abs,
        "acos" => f64::acos,
        "acosh" => c_acosh,
        "asin" => f64::asin,
        "asinh" => c_asinh,
        "atan" => f64::atan,
        "atanh" => c_atanh,
        "floor" => f64::floor,
        "ceil" => f64::ceil,
        "round" => round,
        "trunc" => f64::trunc,
        "sign" => |x| if x.is_nan() || x == 0.0 { x } else if x > 0.0 { 1.0 } else { -1.0 },
        "sqrt" => f64::sqrt,
        "cbrt" => f64::cbrt,
        "cos" => f64::cos,
        "cosh" => f64::cosh,
        "sin" => f64::sin,
        "sinh" => f64::sinh,
        "tan" => f64::tan,
        "tanh" => f64::tanh,
        "log" => f64::ln,
        "log2" => f64::log2,
        "log10" => f64::log10,
        "log1p" => f64::ln_1p,
        "exp" => f64::exp,
        "expm1" => f64::exp_m1,
        "f16round" => f16round,
        "fround" => |x| x as f32 as f64,
        "clz32" => |x| crate::numconv::to_uint32(x).leading_zeros() as f64,
        _ => unreachable!(),
    }
}

const UNARY: [&str; 29] = [
    "abs", "acos", "acosh", "asin", "asinh", "atan", "atanh", "floor", "ceil", "round", "trunc", "sign", "sqrt", "cbrt",
    "cos", "cosh", "sin", "sinh", "tan", "tanh", "log", "log2", "log10", "log1p", "exp", "expm1", "f16round", "fround",
    "clz32",
];

fn numbers(values: &[Value]) -> R<Vec<f64>> {
    values.iter().map(to_number_checked).collect()
}

fn max(values: &[f64]) -> f64 {
    let mut result = f64::NEG_INFINITY;
    for &value in values {
        if value.is_nan() {
            return f64::NAN;
        }
        if value > result || (value == 0.0 && result == 0.0 && result.is_sign_negative()) {
            result = value;
        }
    }
    result
}

fn min(values: &[f64]) -> f64 {
    let mut result = f64::INFINITY;
    for &value in values {
        if value.is_nan() {
            return f64::NAN;
        }
        if value < result || (value == 0.0 && result == 0.0 && value.is_sign_negative()) {
            result = value;
        }
    }
    result
}

fn hypot(values: &[f64]) -> f64 {
    if values.iter().any(|v| v.is_infinite()) {
        return f64::INFINITY;
    }
    if values.iter().any(|v| v.is_nan()) {
        return f64::NAN;
    }
    let largest = values.iter().fold(0.0f64, |acc, v| acc.max(v.abs()));
    if largest == 0.0 {
        return 0.0;
    }
    let mut sum = 0.0;
    let mut compensation = 0.0;
    for v in values {
        let scaled = v / largest;
        let summand = scaled * scaled - compensation;
        let prelim = sum + summand;
        compensation = (prelim - sum) - summand;
        sum = prelim;
    }
    sum.sqrt() * largest
}

/// Math.sumPrecise: the exactly rounded sum (Shewchuk's algorithm over expansions).
fn sum_precise(values: &[f64]) -> f64 {
    let mut partials: Vec<f64> = Vec::new();
    let mut infinity = 0.0f64;
    let mut nan = false;
    let mut all_negative_zero = true;
    for &x in values {
        if x.is_nan() {
            nan = true;
            continue;
        }
        if x.is_infinite() {
            if infinity != 0.0 && infinity != x {
                nan = true;
            }
            infinity = x;
            continue;
        }
        if !(x == 0.0 && x.is_sign_negative()) {
            all_negative_zero = false;
        }
        let mut x = x;
        let mut kept = Vec::with_capacity(partials.len() + 1);
        for &y in &partials {
            let (mut a, mut b) = (x, y);
            if a.abs() < b.abs() {
                std::mem::swap(&mut a, &mut b);
            }
            let hi = a + b;
            let lo = b - (hi - a);
            if lo != 0.0 {
                kept.push(lo);
            }
            x = hi;
        }
        kept.push(x);
        partials = kept;
    }
    if nan {
        return f64::NAN;
    }
    if infinity != 0.0 {
        return infinity;
    }
    if all_negative_zero {
        return -0.0;
    }
    // Round the expansion to the nearest double (msum's final step).
    let mut n = partials.len();
    if n == 0 {
        return 0.0;
    }
    n -= 1;
    let mut hi = partials[n];
    let mut lo = 0.0;
    while n > 0 {
        let x = hi;
        n -= 1;
        let y = partials[n];
        hi = x + y;
        let yr = hi - x;
        lo = y - yr;
        if lo != 0.0 {
            break;
        }
    }
    if n > 0 && ((lo < 0.0 && partials[n - 1] < 0.0) || (lo > 0.0 && partials[n - 1] > 0.0)) {
        let y = lo * 2.0;
        let x = hi + y;
        let yr = x - hi;
        if y == yr {
            hi = x;
        }
    }
    hi + 0.0
}

pub fn math_global(ctx: &Ctx) -> Value {
    let math = ctx.new_object(Some(ctx.builtins.object.clone()), Kind::Namespace("Math"));
    ctx.constants(
        &math,
        &[
            ("PI", Value::Num(std::f64::consts::PI)),
            ("E", Value::Num(std::f64::consts::E)),
            ("LN2", Value::Num(std::f64::consts::LN_2)),
            ("LN10", Value::Num(std::f64::consts::LN_10)),
            ("LOG2E", Value::Num(std::f64::consts::LOG2_E)),
            ("LOG10E", Value::Num(std::f64::consts::LOG10_E)),
            ("SQRT2", Value::Num(std::f64::consts::SQRT_2)),
            ("SQRT1_2", Value::Num(std::f64::consts::FRAC_1_SQRT_2)),
        ],
    );
    ctx.method(&math, "random", 0, |_, _, _, _| {
        let mut bytes = [0u8; 8];
        let _ = getrandom::fill(&mut bytes);
        let bits = u64::from_le_bytes(bytes) >> 11;
        Ok(Value::Num(bits as f64 / (1u64 << 53) as f64))
    });
    for name in ["max", "min", "hypot"] {
        ctx.method_work(&math, name, 2, move |ctx, _, args, _| {
            with_primitives(ctx, &[Hint::Number], args, move |_, values| {
                let values = numbers(&values)?;
                Ok(NOut::Val(Value::Num(match name {
                    "max" => max(&values),
                    "min" => min(&values),
                    _ => hypot(&values),
                })))
            })
        });
    }
    for name in UNARY {
        let op = unary(name);
        ctx.method_work(&math, name, 1, move |ctx, _, args, _| {
            let value = args.first().cloned().unwrap_or_default();
            with_primitives(ctx, &[Hint::Number], vec![value], move |_, values| {
                Ok(NOut::Val(Value::Num(op(to_number_checked(&values[0])?))))
            })
        });
    }
    for name in ["atan2", "pow", "imul"] {
        ctx.method_work(&math, name, 2, move |ctx, _, args, _| {
            let a = args.first().cloned().unwrap_or_default();
            let b = args.get(1).cloned().unwrap_or_default();
            with_primitives(ctx, &[Hint::Number], vec![a, b], move |_, values| {
                let (a, b) = (to_number_checked(&values[0])?, to_number_checked(&values[1])?);
                Ok(NOut::Val(Value::Num(match name {
                    "atan2" => a.atan2(b),
                    "pow" => crate::interp::expr::js_pow(a, b),
                    _ => crate::numconv::to_int32(a).wrapping_mul(crate::numconv::to_int32(b)) as f64,
                })))
            })
        });
    }
    ctx.method_work(&math, "sumPrecise", 1, |ctx, _, args, _| {
        let ctx = ctx.clone();
        Ok(NOut::Work(boxed(async move {
            let source = args.into_iter().next().unwrap_or_default();
            let Some(cursor) = ctx.iterate(&source, None).await? else {
                return Err(type_error("Math.sumPrecise expects a synchronous iterable."));
            };
            let mut values = Vec::new();
            loop {
                let step = cursor.next_step(&ctx).await?;
                if step.done {
                    return Ok(Value::Num(sum_precise(&values)));
                }
                match step.value {
                    Value::Num(n) => values.push(n),
                    _ => {
                        let close = {
                            let (ctx, cursor) = (ctx.clone(), cursor.clone());
                            async move { cursor.close_now(&ctx).await }
                        };
                        return preserve_consumer_error(close, Err(type_error("Math.sumPrecise expects an iterable of numbers.")))
                            .await;
                    }
                }
            }
        })))
    });
    Value::Obj(math)
}
