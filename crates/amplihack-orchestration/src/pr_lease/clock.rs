//! Injected time source for the PR-ownership lease.
//!
//! The acquire, expiry, and `assert_owned` paths call [`Clock::now`] rather than
//! `Utc::now()` directly, so TTL expiry is testable without real sleeps.

use std::time::Duration;

use chrono::{DateTime, Utc};

/// A source of the current time.
pub trait Clock {
    /// Current wall-clock time as a UTC instant.
    fn now(&self) -> DateTime<Utc>;
}

/// Production clock: delegates to `Utc::now()`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Test clock that is advanced manually. Interior-mutable so `advance`/`set`
/// take `&self` and can be shared with a borrowing lease handle.
#[derive(Debug)]
pub struct MockClock {
    now: std::sync::Mutex<DateTime<Utc>>,
}

impl MockClock {
    /// Create a clock frozen at `start`.
    pub fn new(start: DateTime<Utc>) -> Self {
        Self {
            now: std::sync::Mutex::new(start),
        }
    }

    /// Move time forward by `by`.
    pub fn advance(&self, by: Duration) {
        let delta = chrono::Duration::from_std(by)
            .unwrap_or_else(|_| chrono::Duration::seconds(by.as_secs() as i64));
        let mut guard = self.now.lock().expect("MockClock mutex poisoned");
        *guard += delta;
    }

    /// Set the clock to an absolute instant.
    pub fn set(&self, to: DateTime<Utc>) {
        *self.now.lock().expect("MockClock mutex poisoned") = to;
    }
}

impl Clock for MockClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().expect("MockClock mutex poisoned")
    }
}
