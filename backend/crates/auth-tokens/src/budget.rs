//! Process-wide ESI error-budget gauge.
//!
//! ESI imposes a 100-non-2xx-per-minute ceiling on a single client identity.
//! Each per-character `EsiClient` maintains its own internal counter, which
//! fragments that ceiling across many character sessions. To keep workers from
//! collectively blowing past it, batches consult this guard before kicking off
//! and skip the tick if remaining budget falls below the threshold.

use std::sync::atomic::{AtomicI16, Ordering};

const DEFAULT_CEILING: i16 = 100;
const DEFAULT_DEFER_THRESHOLD: i16 = 20;

#[derive(Clone)]
pub struct EsiBudgetGuard {
    inner: std::sync::Arc<Inner>,
}

struct Inner {
    /// Estimated remaining non-2xx responses before ESI starts 420ing us.
    remaining: AtomicI16,
    /// If `remaining < threshold` callers should defer.
    defer_threshold: i16,
    ceiling: i16,
}

impl Default for EsiBudgetGuard {
    fn default() -> Self {
        Self::new(DEFAULT_CEILING, DEFAULT_DEFER_THRESHOLD)
    }
}

impl EsiBudgetGuard {
    pub fn new(ceiling: i16, defer_threshold: i16) -> Self {
        Self {
            inner: std::sync::Arc::new(Inner {
                remaining: AtomicI16::new(ceiling),
                defer_threshold,
                ceiling,
            }),
        }
    }

    /// True if the guard has enough budget left to kick off another batch.
    pub fn has_budget(&self) -> bool {
        self.inner.remaining.load(Ordering::Relaxed) >= self.inner.defer_threshold
    }

    /// Snapshot of the current remaining count.
    pub fn remaining(&self) -> i16 {
        self.inner.remaining.load(Ordering::Relaxed)
    }

    /// Decrement on a non-2xx response, clamped at zero.
    pub fn record_non_2xx(&self) {
        let _ = self
            .inner
            .remaining
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                if v > 0 {
                    Some(v - 1)
                } else {
                    None
                }
            });
    }

    /// Restore to the ceiling (called periodically from the worker once a
    /// minute, mirroring ESI's reset window).
    pub fn reset(&self) {
        self.inner
            .remaining
            .store(self.inner.ceiling, Ordering::Relaxed);
    }
}

/// Run an ESI future under budget accounting: on `Err`, decrement the
/// guard before returning. New ESI call sites should always go through
/// this rather than calling `record_non_2xx` by hand.
///
/// ```ignore
/// let row = budgeted(&ctx.budget, esi.get_contracts(char_id)).await?;
/// ```
pub async fn budgeted<T, E, F>(guard: &EsiBudgetGuard, fut: F) -> Result<T, E>
where
    F: std::future::Future<Output = Result<T, E>>,
{
    match fut.await {
        Ok(v) => Ok(v),
        Err(e) => {
            guard.record_non_2xx();
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defers_below_threshold() {
        let g = EsiBudgetGuard::new(10, 5);
        assert!(g.has_budget());
        for _ in 0..5 {
            g.record_non_2xx();
        }
        // 10 - 5 = 5 == threshold, still OK
        assert!(g.has_budget());
        g.record_non_2xx();
        assert!(!g.has_budget());
        g.reset();
        assert!(g.has_budget());
        assert_eq!(g.remaining(), 10);
    }

    #[test]
    fn record_non_2xx_floors_at_zero() {
        let g = EsiBudgetGuard::new(3, 1);
        for _ in 0..10 {
            g.record_non_2xx();
        }
        assert_eq!(g.remaining(), 0, "must not drift below zero");
    }

    #[tokio::test]
    async fn budgeted_passthrough_on_ok() {
        let g = EsiBudgetGuard::new(10, 5);
        let v: Result<i32, &str> = budgeted(&g, async { Ok(42) }).await;
        assert_eq!(v.unwrap(), 42);
        assert_eq!(g.remaining(), 10, "Ok path must not decrement");
    }

    #[tokio::test]
    async fn budgeted_decrements_on_err() {
        let g = EsiBudgetGuard::new(10, 5);
        let v: Result<i32, &str> = budgeted(&g, async { Err("boom") }).await;
        assert!(v.is_err());
        assert_eq!(g.remaining(), 9);

        // Each Err decrements by exactly one.
        let _: Result<i32, &str> = budgeted(&g, async { Err("again") }).await;
        assert_eq!(g.remaining(), 8);
    }

    #[tokio::test]
    async fn budgeted_preserves_error_value() {
        let g = EsiBudgetGuard::new(10, 5);
        let v: Result<(), String> =
            budgeted(&g, async { Err("specific message".to_string()) }).await;
        assert_eq!(v.unwrap_err(), "specific message");
    }
}
