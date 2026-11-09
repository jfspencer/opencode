//! `retry(operation, options)` (packages/util/src/retry.ts): up to `attempts` tries, waiting
//! `min(delay * factor^attempt, maxDelay)` between them, while `retryIf` (by default: a transient network error)
//! holds.

use std::rc::Rc;

use crate::client::Error;
use crate::data::Promise;
use crate::runtime::{self, Deferred};
use crate::transport::LocalBoxFuture;

pub struct RetryOptions {
    pub attempts: u32,
    pub delay: f64,
    pub factor: f64,
    pub max_delay: f64,
    pub retry_if: Rc<dyn Fn(&Error) -> bool>,
}

impl Default for RetryOptions {
    fn default() -> Self {
        Self { attempts: 3, delay: 500.0, factor: 2.0, max_delay: 10_000.0, retry_if: Rc::new(is_transient_error) }
    }
}

const TRANSIENT: [&str; 9] = [
    "load failed",
    "network connection was lost",
    "network request failed",
    "failed to fetch",
    "econnreset",
    "econnrefused",
    "epipe",
    "etimedout",
    "socket hang up",
];

/// `isTransientError(error)`: the error's message, or its cause's.
pub fn is_transient_error(error: &Error) -> bool {
    let transient = |text: &str| {
        let text = format!("{text} ").to_lowercase();
        TRANSIENT.iter().any(|item| text.contains(item))
    };
    if transient(&error.message()) {
        return true;
    }
    match error {
        Error::Client { cause: Some(cause), .. } => transient(&cause.message),
        _ => false,
    }
}

/// Runs `operation` until it succeeds, retries run out, or `retry_if` refuses. The first attempt starts at the
/// call, as the TS async function's does.
pub fn retry<T: Clone + 'static>(
    operation: impl Fn() -> LocalBoxFuture<'static, Result<T, Error>> + 'static,
    options: RetryOptions,
) -> Promise<T> {
    let first = operation();
    Deferred::spawn(async move {
        let attempts = options.attempts.max(1);
        let mut attempt = 0;
        let mut running = first;
        loop {
            match running.await {
                Ok(value) => return Ok(value),
                Err(error) => {
                    if attempt == attempts - 1 || !(options.retry_if)(&error) {
                        return Err(error);
                    }
                    runtime::sleep((options.delay * options.factor.powi(attempt as i32)).min(options.max_delay)).await;
                    attempt += 1;
                    running = operation();
                }
            }
        }
    })
}
