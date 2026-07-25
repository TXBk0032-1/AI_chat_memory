//! Axum serve/stop helpers for a single local service instance.

use super::LocalServiceStatus;
use axum::Router;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub(crate) struct RunningService {
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}

impl RunningService {
    /// True while the serve task has not finished (Ok or Err).
    pub(crate) fn is_alive(&self) -> bool {
        !self.handle.is_finished()
    }

    #[cfg(test)]
    pub(crate) async fn finished_for_test() -> Self {
        let handle = tokio::spawn(async {});
        while !handle.is_finished() {
            tokio::task::yield_now().await;
        }
        Self {
            cancel: CancellationToken::new(),
            handle,
        }
    }
}

/// Bind, mark Running, then serve with graceful shutdown on `cancel`.
pub(crate) async fn start(
    bind: SocketAddr,
    app: Router,
    status: Arc<Mutex<LocalServiceStatus>>,
) -> Result<RunningService, String> {
    {
        let mut slot = status.lock().await;
        *slot = LocalServiceStatus::Starting;
    }

    let listener = TcpListener::bind(bind)
        .await
        .map_err(|error| error.to_string())?;

    {
        let mut slot = status.lock().await;
        *slot = LocalServiceStatus::Running;
    }

    let cancel = CancellationToken::new();
    let shutdown = cancel.clone();
    let status_for_task = Arc::clone(&status);

    let handle = tokio::spawn(async move {
        let serve_result = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                shutdown.cancelled().await;
            })
            .await;

        if let Err(error) = serve_result {
            let mut slot = status_for_task.lock().await;
            *slot = LocalServiceStatus::Failed(error.to_string());
        }
    });

    Ok(RunningService { cancel, handle })
}

/// Cancel the serve task and wait up to 5s for the listener to release.
pub(crate) async fn stop(running: RunningService, status: Arc<Mutex<LocalServiceStatus>>) {
    stop_with_timeout(running, status, Duration::from_secs(5)).await;
}

async fn stop_with_timeout(
    running: RunningService,
    status: Arc<Mutex<LocalServiceStatus>>,
    timeout: Duration,
) {
    let RunningService { cancel, mut handle } = running;
    cancel.cancel();
    if tokio::time::timeout(timeout, &mut handle).await.is_err() {
        handle.abort();
        let _ = handle.await;
    }
    let mut slot = status.lock().await;
    *slot = LocalServiceStatus::Stopped;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::oneshot;

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn stop_timeout_aborts_task_before_marking_stopped() {
        let cancel = CancellationToken::new();
        let shutdown = cancel.clone();
        let status = Arc::new(Mutex::new(LocalServiceStatus::Running));
        let dropped = Arc::new(AtomicBool::new(false));
        let cancellation_observed = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = oneshot::channel();

        let dropped_for_task = Arc::clone(&dropped);
        let cancellation_observed_for_task = Arc::clone(&cancellation_observed);
        let handle = tokio::spawn(async move {
            let _drop_flag = DropFlag(dropped_for_task);
            ready_tx.send(()).expect("test must still be waiting");
            shutdown.cancelled().await;
            cancellation_observed_for_task.store(true, Ordering::SeqCst);
            std::future::pending::<()>().await;
        });
        ready_rx.await.expect("task must start");

        stop_with_timeout(
            RunningService { cancel, handle },
            Arc::clone(&status),
            Duration::from_millis(25),
        )
        .await;

        assert!(
            cancellation_observed.load(Ordering::SeqCst),
            "graceful shutdown must be observed before the timeout"
        );
        assert!(
            dropped.load(Ordering::SeqCst),
            "timed-out task must be destroyed before stop returns"
        );
        assert_eq!(*status.lock().await, LocalServiceStatus::Stopped);
    }
}
