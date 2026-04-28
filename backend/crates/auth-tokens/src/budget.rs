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

    /// Decrement on a non-2xx response.
    pub fn record_non_2xx(&self) {
        self.inner.remaining.fetch_sub(1, Ordering::Relaxed);
    }

    /// Restore to the ceiling (called periodically from the worker once a
    /// minute, mirroring ESI's reset window).
    pub fn reset(&self) {
        self.inner
            .remaining
            .store(self.inner.ceiling, Ordering::Relaxed);
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
}
