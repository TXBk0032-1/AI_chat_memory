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
    running.cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), running.handle).await;
    let mut slot = status.lock().await;
    *slot = LocalServiceStatus::Stopped;
}
