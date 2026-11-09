//! What one built-in may build (limits.ts): the timeout fires only between interpreter steps.

use crate::interp::R;
use crate::interp::errors::range_error;

/// Longest string a built-in or operator may produce.
pub const MAX_STRING_LENGTH: usize = 1 << 24;
/// Longest array a built-in may create or grow to.
pub const MAX_ARRAY_LENGTH: usize = 10_000_000;
/// Most promises that may be pending at once.
pub const MAX_PENDING_PROMISES: usize = 10_000;
/// Deepest nesting a value may have when it crosses to or from the host.
pub const MAX_VALUE_DEPTH: usize = 32;
/// Most activations one task may hold.
pub const MAX_CALL_DEPTH: usize = 10_000;

pub fn check_string_length(length: f64) -> R<()> {
    if length > MAX_STRING_LENGTH as f64 {
        return Err(range_error("Invalid string length"));
    }
    Ok(())
}

pub fn check_array_length(length: f64) -> R<()> {
    if length > MAX_ARRAY_LENGTH as f64 {
        return Err(range_error("Invalid array length"));
    }
    Ok(())
}
