//! The model retry policy (packages/core/src/model-retry.ts, session/runner/retry.ts): exponential backoff from
//! 2 s capped at 10 s per gap, at most 10 retries, jittered (×0.8–1.2), never shorter than what a rate limit or
//! provider failure asked for (at most 15 minutes); failures that say the provider's host is unreachable retry on
//! their own budget (2 s to 30 s gaps, for an hour); at most 3 timed-out attempts retry.
//!
//! The `session.retry` plugin hook is out of scope: no built-in plugin registers it, only external plugins do.

use crate::llm::AIError;
use crate::llm::error::Kind;

#[derive(Debug, Clone)]
pub struct Decision {
    pub retry: bool,
    pub attempt: i64,
    pub delay: f64,
}

pub struct Retry {
    attempt: i64,
    /// Retries taken on the general schedule.
    recurrences: u32,
    unreachable_since: Option<i64>,
    unreachable_recurrences: u32,
    timeouts: u32,
}

impl Default for Retry {
    fn default() -> Self {
        Self { attempt: 1, recurrences: 0, unreachable_since: None, unreachable_recurrences: 0, timeouts: 0 }
    }
}

fn jitter(delay: f64) -> f64 {
    let mut bytes = [0u8; 8];
    crate::id::random(&mut bytes);
    let unit = (u64::from_le_bytes(bytes) >> 11) as f64 / (1u64 << 53) as f64;
    delay * (0.8 + 0.4 * unit)
}

impl Retry {
    /// `ModelRetry.policy`'s decision for one failure: `None` once the schedule is exhausted.
    pub fn decide(&mut self, cause: &AIError, retry: bool) -> Option<Decision> {
        let now = crate::id::now();
        let delay = if retry && cause.unreachable() {
            let since = *self.unreachable_since.get_or_insert(now);
            if now - since > 3_600_000 {
                return None;
            }
            let gap = (2_000.0 * 2f64.powi(self.unreachable_recurrences as i32)).min(30_000.0);
            self.unreachable_recurrences += 1;
            jitter(gap).ceil().min(30_000.0)
        } else {
            if self.recurrences >= 10 {
                return None;
            }
            let gap = (2_000.0 * 2f64.powi(self.recurrences as i32)).min(10_000.0);
            self.recurrences += 1;
            let delay = jitter(gap);
            let delay = match cause.retry_after() {
                Some(minimum) => delay.max(minimum),
                None => delay,
            };
            delay.ceil()
        };
        self.attempt += 1;
        if cause.kind == Kind::Transport && cause.code.as_deref() == Some("Timeout") {
            self.timeouts += 1;
        }
        Some(Decision { retry: retry && self.timeouts <= 3, attempt: self.attempt, delay })
    }
}
