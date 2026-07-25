//! MCP Streamable HTTP router factory (wired by task 4+).
#![allow(dead_code)]

use crate::mcp::server::ChatMemoryMcp;
use crate::service::AppService;
use axum::Json;
use axum::Router;
use axum::routing::get;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde_json::{Value, json};
use std::sync::Arc;

/// Build the MCP Axum router: `GET /health` + Streamable HTTP at `/mcp`.
pub fn build_mcp_router(app_service: AppService) -> Router {
    let service = StreamableHttpService::new(
        {
            let app_service = app_service.clone();
            move || Ok(ChatMemoryMcp::new(app_service.clone()))
        },
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_stateful_mode(false)
            .with_json_response(true),
    );

    Router::new()
        .route("/health", get(health))
        .nest_service("/mcp", service)
}

async fn health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "ai-chat-memory-mcp",
    }))
}

#[cfg(test)]
mod tests {
    use super::health;
    use crate::local_services::{
        LocalServiceId, LocalServiceManager, LocalServiceSpec, LocalServiceStatus,
    };
    use axum::{Router, routing::get};
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;

    /// Avoid production 19820/19821.
    const TEST_PORT: u16 = 19991;

    fn health_only_router() -> Router {
        // Degradation path: health only (no AppService / embedding init).
        Router::new().route("/health", get(health))
    }

    async fn port_open(port: u16) -> bool {
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
    }

    #[tokio::test]
    async fn mcp_health_probe_via_manager_start_and_stop() {
        let manager = LocalServiceManager::new();
        manager
            .register(LocalServiceSpec {
                id: LocalServiceId::Mcp,
                bind: SocketAddr::from(([127, 0, 0, 1], TEST_PORT)),
                build: Arc::new(health_only_router),
            })
            .await;

        manager.apply_desired(LocalServiceId::Mcp, true).await;
        let status = manager.status(LocalServiceId::Mcp).await;
        assert!(
            matches!(status, LocalServiceStatus::Running),
            "expected Running, got {status:?}"
        );
        assert!(
            port_open(TEST_PORT).await,
            "MCP test port must accept while running"
        );

        let url = format!("http://127.0.0.1:{TEST_PORT}/health");
        let body: serde_json::Value = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .expect("GET /health")
            .error_for_status()
            .expect("health status")
            .json()
            .await
            .expect("health json");
        assert_eq!(body["status"], "ok");
        assert_eq!(body["service"], "ai-chat-memory-mcp");

        manager.apply_desired(LocalServiceId::Mcp, false).await;
        let status = manager.status(LocalServiceId::Mcp).await;
        assert!(
            matches!(status, LocalServiceStatus::Stopped),
            "expected Stopped, got {status:?}"
        );

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !port_open(TEST_PORT).await,
            "port must be released after MCP manager stop"
        );
    }
}

