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

/// An unsigned big integer, little-endian 64-bit limbs.
#[derive(Clone, Default)]
struct Big(Vec<u64>);

impl Big {
    fn add_shifted(&mut self, mantissa: u64, shift: u32) {
        let limb = (shift / 64) as usize;
        let bit = shift % 64;
        let wide = (mantissa as u128) << bit;
        let parts = [wide as u64, (wide >> 64) as u64];
        if self.0.len() < limb + 3 {
            self.0.resize(limb + 3, 0);
        }
        let mut carry = 0u128;
        let mut index = limb;
        for part in parts {
            let sum = self.0[index] as u128 + part as u128 + carry;
            self.0[index] = sum as u64;
            carry = sum >> 64;
            index += 1;
        }
        while carry > 0 {
            if index == self.0.len() {
                self.0.push(0);
            }
            let sum = self.0[index] as u128 + carry;
            self.0[index] = sum as u64;
            carry = sum >> 64;
            index += 1;
        }
    }
    fn trim(&mut self) {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
    }
    fn cmp(&self, other: &Big) -> std::cmp::Ordering {
        let (mut a, mut b) = (self.clone(), other.clone());
        a.trim();
        b.trim();
        a.0.len().cmp(&b.0.len()).then_with(|| a.0.iter().rev().cmp(b.0.iter().rev()))
    }
    fn sub(&self, other: &Big) -> Big {
        let mut out = self.0.clone();
        let mut borrow = 0i128;
        for (index, limb) in out.iter_mut().enumerate() {
            let rhs = *other.0.get(index).unwrap_or(&0) as i128;
            let mut diff = *limb as i128 - rhs - borrow;
            borrow = 0;
            if diff < 0 {
                diff += 1i128 << 64;
                borrow = 1;
            }
            *limb = diff as u64;
        }
        let mut big = Big(out);
        big.trim();
        big
    }
    fn bit_len(&self) -> u32 {
        for (index, limb) in self.0.iter().enumerate().rev() {
            if *limb != 0 {
                return index as u32 * 64 + (64 - limb.leading_zeros());
            }
        }
        0
    }
    fn bit(&self, at: u32) -> bool {
        let limb = (at / 64) as usize;
        self.0.get(limb).is_some_and(|l| (l >> (at % 64)) & 1 == 1)
    }
    /// The value times 2^-1074, rounded to the nearest double, ties to even.
    fn to_f64(&self) -> f64 {
        let length = self.bit_len();
        if length == 0 {
            return 0.0;
        }
        if length <= 53 {
            let mut mantissa = 0u64;
            for at in (0..length).rev() {
                mantissa = (mantissa << 1) | self.bit(at) as u64;
            }
            return mantissa as f64 * 2f64.powi(-1074);
        }
        let drop = length - 53;
        let mut mantissa = 0u64;
        for at in (drop..length).rev() {
            mantissa = (mantissa << 1) | self.bit(at) as u64;
        }
        let half = self.bit(drop - 1);
        let sticky = (0..drop - 1).any(|at| self.bit(at));
        if half && (sticky || mantissa & 1 == 1) {
            mantissa += 1;
        }
        let mut exponent = drop as i32 - 1074;
        if mantissa == 1u64 << 53 {
            mantissa >>= 1;
            exponent += 1;
        }
        // At least 2^53 units of 2^-1074, so the result is normal: build it from its fields.
        let biased = exponent + 52 + 1023;
        if biased >= 2047 {
            return f64::INFINITY;
        }
        f64::from_bits(((biased as u64) << 52) | (mantissa & ((1u64 << 52) - 1)))
    }
}

/// Math.sumPrecise: the exact sum of the values, rounded once to the nearest double.
fn sum_precise(values: &[f64]) -> f64 {
    let mut positive = Big::default();
    let mut negative = Big::default();
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
        if x == 0.0 {
            continue;
        }
        let bits = x.to_bits();
        let exponent = ((bits >> 52) & 0x7FF) as u32;
        let fraction = bits & ((1u64 << 52) - 1);
        let (mantissa, shift) = if exponent == 0 { (fraction, 0) } else { (fraction | (1u64 << 52), exponent - 1) };
        if x < 0.0 {
            negative.add_shifted(mantissa, shift);
        } else {
            positive.add_shifted(mantissa, shift);
        }
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
    match positive.cmp(&negative) {
        std::cmp::Ordering::Equal => 0.0,
        std::cmp::Ordering::Greater => positive.sub(&negative).to_f64(),
        std::cmp::Ordering::Less => -negative.sub(&positive).to_f64(),
    }
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
