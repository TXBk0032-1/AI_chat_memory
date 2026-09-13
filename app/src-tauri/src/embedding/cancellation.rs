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
    /// Wake-up channel only. The atomic generation is the single source of
    /// truth for "is this token cancelled?"; the channel carries no value so
    /// concurrent `cancel_current()` calls cannot leave a stale one behind.
    changed: watch::Sender<()>,
}

#[derive(Clone)]
pub struct CancellationToken {
    generation: u64,
    current: Arc<AtomicU64>,
    changed: watch::Receiver<()>,
}

impl CancellationSource {
    pub fn new() -> Self {
        let (changed, _) = watch::channel(());
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
        self.generation.fetch_add(1, Ordering::SeqCst);
        // Wake waiters; they re-check the atomic, so the order between the
        // bump and the wake-up is all that matters here.
        self.changed.send_replace(());
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
    ///
    /// Invariant: this future completes if and only if `is_cancelled()` is
    /// true. If every `CancellationSource` has been dropped, nothing can ever
    /// cancel this generation any more, so the future stays pending forever
    /// rather than reporting a cancellation that never happened. Callers
    /// racing it against real work in `select!` therefore never see a
    /// spurious abort.
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
        // All sources dropped: no cancellation can arrive. Keep the strict
        // "only a real cancel resolves" contract.
        std::future::pending::<()>().await
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

    #[tokio::test]
    async fn dropping_every_source_does_not_count_as_cancellation() {
        let source = CancellationSource::new();
        let token = source.token();
        let cloned = source.clone();
        drop(source);
        drop(cloned);
        let waited =
            tokio::time::timeout(std::time::Duration::from_millis(50), token.cancelled()).await;
        assert!(
            waited.is_err(),
            "cancelled() must stay pending when no source can ever cancel"
        );
        assert!(!token.is_cancelled());
    }

    #[tokio::test]
    async fn waiter_started_before_cancel_wakes_on_concurrent_cancels() {
        let source = CancellationSource::new();
        let token = source.token();
        let waiter = tokio::spawn({
            let token = token.clone();
            async move { token.cancelled().await }
        });
        tokio::task::yield_now().await;
        let a = source.clone();
        let b = source.clone();
        let (ra, rb) = tokio::join!(
            tokio::task::spawn_blocking(move || a.cancel_current()),
            tokio::task::spawn_blocking(move || b.cancel_current()),
        );
        ra.unwrap();
        rb.unwrap();
        tokio::time::timeout(std::time::Duration::from_millis(100), waiter)
            .await
            .expect("waiter must wake after concurrent cancels")
            .unwrap();
        assert!(token.is_cancelled());
        assert!(!source.token().is_cancelled());
    }
}
