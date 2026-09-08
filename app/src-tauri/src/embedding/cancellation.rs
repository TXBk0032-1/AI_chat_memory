//! Generation-based cancellation for background embedding work.
//!
//! A [`CancellationSource`] hands out [`CancellationToken`]s stamped with the
//! generation current at issue time. `cancel_current()` bumps the generation,
//! which invalidates every token issued so far; tokens issued afterwards read
//! the new generation and are unaffected. Nothing ever "clears" a
//! cancellation, so old work can never be revived by new work starting.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::watch;

#[derive(Clone)]
pub struct CancellationSource {
    generation: Arc<AtomicU64>,
    changed: watch::Sender<u64>,
}

#[derive(Clone)]
pub struct CancellationToken {
    generation: u64,
    current: Arc<AtomicU64>,
    changed: watch::Receiver<u64>,
}

impl CancellationSource {
    pub fn new() -> Self {
        let (changed, _) = watch::channel(0);
        Self {
            generation: Arc::new(AtomicU64::new(0)),
            changed,
        }
    }

    /// Issues a token bound to the current generation.
    pub fn token(&self) -> CancellationToken {
        CancellationToken {
            generation: self.generation.load(Ordering::SeqCst),
            current: Arc::clone(&self.generation),
            changed: self.changed.subscribe(),
        }
    }

    /// Invalidates every token issued so far and wakes their waiters. Tokens
    /// issued afterwards belong to the new generation and stay live.
    pub fn cancel_current(&self) {
        let next = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.changed.send_replace(next);
    }

    /// Alias of [`token`](Self::token) that documents intent at call sites
    /// starting a new unit of background work. It never bumps the generation,
    /// so it cannot revive tokens that were already cancelled.
    pub fn begin_work(&self) -> CancellationToken {
        self.token()
    }
}

impl Default for CancellationSource {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    pub fn is_cancelled(&self) -> bool {
        self.current.load(Ordering::SeqCst) != self.generation
    }

    /// Resolves once this token's generation has been cancelled.
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let mut changed = self.changed.clone();
        while changed.changed().await.is_ok() {
            if self.is_cancelled() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelling_one_generation_does_not_cancel_the_next() {
        let source = CancellationSource::new();
        let first = source.token();
        source.cancel_current();
        assert!(first.is_cancelled());
        let second = source.begin_work();
        assert!(!second.is_cancelled());
        assert!(
            first.is_cancelled(),
            "starting new work must not revive old work"
        );
    }

    #[tokio::test]
    async fn cancelled_waiter_wakes() {
        let source = CancellationSource::new();
        let token = source.token();
        source.cancel_current();
        tokio::time::timeout(std::time::Duration::from_millis(100), token.cancelled())
            .await
            .expect("cancelled token must wake")
    }
}
