//! Hot-start/stop manager for loopback local services (MCP, etc.).
//! Public API is consumed by later MCP wiring tasks.

#![allow(dead_code)]

mod runtime;

use serde::Serialize;
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LocalServiceId {
    Mcp,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "state", content = "message", rename_all = "snake_case")]
pub enum LocalServiceStatus {
    Starting,
    Running,
    Stopped,
    Failed(String),
}

pub struct LocalServiceSpec {
    pub id: LocalServiceId,
    pub bind: SocketAddr,
    pub build: Arc<dyn Fn() -> axum::Router + Send + Sync>,
}

struct RegisteredService {
    bind: SocketAddr,
    build: Arc<dyn Fn() -> axum::Router + Send + Sync>,
    status: Arc<Mutex<LocalServiceStatus>>,
    running: Option<runtime::RunningService>,
}

/// Serializes `apply_desired` per process via a single mutex (one MCP service today).
pub struct LocalServiceManager {
    services: Mutex<HashMap<LocalServiceId, RegisteredService>>,
}

impl LocalServiceManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            services: Mutex::new(HashMap::new()),
        })
    }

    pub async fn register(&self, spec: LocalServiceSpec) {
        let mut services = self.services.lock().await;
        services.insert(
            spec.id,
            RegisteredService {
                bind: spec.bind,
                build: spec.build,
                status: Arc::new(Mutex::new(LocalServiceStatus::Stopped)),
                running: None,
            },
        );
    }

    pub async fn apply_desired(&self, id: LocalServiceId, enabled: bool) {
        let mut services = self.services.lock().await;
        let Some(service) = services.get_mut(&id) else {
            return;
        };

        if enabled {
            // Alive serve: already up. Finished handle (e.g. serve error -> Failed)
            // must be cleared or re-enable becomes a no-op.
            if let Some(running) = service.running.as_ref()
                && running.is_alive()
            {
                return;
            }
            let _ = service.running.take();

            let app = (service.build)();
            let status = Arc::clone(&service.status);
            match runtime::start(service.bind, app, status).await {
                Ok(running) => {
                    service.running = Some(running);
                }
                Err(message) => {
                    let mut slot = service.status.lock().await;
                    *slot = LocalServiceStatus::Failed(message);
                }
            }
            return;
        }

        if let Some(running) = service.running.take() {
            let status = Arc::clone(&service.status);
            runtime::stop(running, status).await;
            return;
        }

        let mut slot = service.status.lock().await;
        *slot = LocalServiceStatus::Stopped;
    }

    pub async fn status(&self, id: LocalServiceId) -> LocalServiceStatus {
        let services = self.services.lock().await;
        let Some(service) = services.get(&id) else {
            return LocalServiceStatus::Stopped;
        };
        service.status.lock().await.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;

    fn test_router() -> Router {
        Router::new().route("/health", get(|| async { "ok" }))
    }

    async fn port_open(port: u16) -> bool {
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
    }

    #[tokio::test]
    async fn apply_desired_starts_and_stops_listener() {
        let manager = LocalServiceManager::new();
        manager
            .register(LocalServiceSpec {
                id: LocalServiceId::Mcp,
                bind: SocketAddr::from(([127, 0, 0, 1], 19899)),
                build: Arc::new(test_router),
            })
            .await;

        manager.apply_desired(LocalServiceId::Mcp, true).await;
        let status = manager.status(LocalServiceId::Mcp).await;
        assert!(matches!(status, LocalServiceStatus::Running), "{status:?}");

        assert!(
            port_open(19899).await,
            "listener must accept connections while running"
        );

        manager.apply_desired(LocalServiceId::Mcp, false).await;
        let status = manager.status(LocalServiceId::Mcp).await;
        assert!(matches!(status, LocalServiceStatus::Stopped), "{status:?}");

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !port_open(19899).await,
            "port must be released after stop"
        );
    }

    #[tokio::test]
    async fn rapid_toggle_is_serialized_and_ends_stopped() {
        let manager = LocalServiceManager::new();
        manager
            .register(LocalServiceSpec {
                id: LocalServiceId::Mcp,
                bind: SocketAddr::from(([127, 0, 0, 1], 19898)),
                build: Arc::new(test_router),
            })
            .await;
        for _ in 0..5 {
            manager.apply_desired(LocalServiceId::Mcp, true).await;
            manager.apply_desired(LocalServiceId::Mcp, false).await;
        }
        assert!(matches!(
            manager.status(LocalServiceId::Mcp).await,
            LocalServiceStatus::Stopped
        ));
    }

    /// After serve dies (status Failed, running holds a finished handle),
    /// apply_desired(true) must start again — not early-return as already running.
    #[tokio::test]
    async fn apply_desired_true_recovers_after_failed_finished_handle() {
        let manager = LocalServiceManager::new();
        manager
            .register(LocalServiceSpec {
                id: LocalServiceId::Mcp,
                bind: SocketAddr::from(([127, 0, 0, 1], 19897)),
                build: Arc::new(test_router),
            })
            .await;

        {
            let mut services = manager.services.lock().await;
            let service = services.get_mut(&LocalServiceId::Mcp).expect("registered");
            *service.status.lock().await =
                LocalServiceStatus::Failed("simulated serve error".into());
            service.running = Some(runtime::RunningService::finished_for_test().await);
        }

        assert!(matches!(
            manager.status(LocalServiceId::Mcp).await,
            LocalServiceStatus::Failed(_)
        ));

        manager.apply_desired(LocalServiceId::Mcp, true).await;
        let status = manager.status(LocalServiceId::Mcp).await;
        assert!(
            matches!(status, LocalServiceStatus::Running),
            "expected Running after recover, got {status:?}"
        );
        assert!(
            port_open(19897).await,
            "listener must accept after re-enable"
        );

        manager.apply_desired(LocalServiceId::Mcp, false).await;
        assert!(matches!(
            manager.status(LocalServiceId::Mcp).await,
            LocalServiceStatus::Stopped
        ));
    }
}
