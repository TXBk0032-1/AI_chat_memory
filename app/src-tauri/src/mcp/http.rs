//! MCP Streamable HTTP router factory (wired by task 4+).
#![allow(dead_code)]

use crate::mcp::server::ChatMemoryMcp;
use crate::models::AppSettings;
use crate::service::AppService;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde_json::{Value, json};
use std::sync::Arc;

const SECRET_HEADER: &str = "x-ai-chat-memory-secret";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpRejection {
    Disabled,
    OriginNotAllowed,
    MissingSecret,
}

/// MCP 鉴权策略（fail-closed）：端点未启用一律拒绝；带 Origin 的浏览器请求必须命中
/// 白名单；无论是否带 Origin 都必须携带有效 secret。HTTP-2：无 Origin 的本机进程不能
/// 因 secret_enabled=false 而免密；HTTP-1：浏览器请求不能依赖 secret_enabled 开关。
/// OPTIONS 预检无法携带自定义头，白名单命中后放行。
fn mcp_rejection_reason(
    settings: &AppSettings,
    method: Method,
    origin: Option<&str>,
    provided_secret: Option<&str>,
) -> Option<McpRejection> {
    if !settings.mcp_enabled {
        return Some(McpRejection::Disabled);
    }
    if let Some(origin) = origin
        && !settings
            .allowed_origins
            .iter()
            .any(|allowed| allowed == origin)
    {
        return Some(McpRejection::OriginNotAllowed);
    }
    if method == Method::OPTIONS {
        return None;
    }
    let configured_secret = settings.secret.as_deref().unwrap_or_default();
    let authorized = match provided_secret {
        Some(token) if !configured_secret.is_empty() => constant_time_eq(token, configured_secret),
        _ => false,
    };
    if !authorized {
        return Some(McpRejection::MissingSecret);
    }
    None
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    use sha2::{Digest, Sha256};
    let a_hash = Sha256::digest(a.as_bytes());
    let b_hash = Sha256::digest(b.as_bytes());
    let mut diff = 0u8;
    for (x, y) in a_hash.iter().zip(b_hash.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

async fn authorize_mcp(
    State(service): State<AppService>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if path == "/health" {
        return next.run(request).await;
    }

    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    let settings = service.settings().await;
    let method = request.method().clone();

    let header_secret = headers.get(SECRET_HEADER).and_then(|v| v.to_str().ok());
    let bearer_secret = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| {
            auth.strip_prefix("Bearer ")
                .or_else(|| auth.strip_prefix("bearer "))
        });
    let provided_secret = header_secret.or(bearer_secret);

    match mcp_rejection_reason(&settings, method.clone(), origin, provided_secret) {
        Some(McpRejection::Disabled) => {
            tracing::warn!(
                path,
                "MCP request rejected: MCP server is disabled in settings"
            );
            return (StatusCode::FORBIDDEN, "mcp_disabled").into_response();
        }
        Some(McpRejection::OriginNotAllowed) => {
            tracing::warn!(%method, path, origin = origin.unwrap_or("<none>"), "MCP request rejected: origin not allowed");
            return (StatusCode::FORBIDDEN, "origin_not_allowed").into_response();
        }
        Some(McpRejection::MissingSecret) => {
            tracing::warn!(%method, path, is_browser_request = origin.is_some(), "MCP request rejected: missing or invalid secret");
            return (StatusCode::UNAUTHORIZED, "invalid or missing MCP secret").into_response();
        }
        None => {}
    }

    if method == Method::OPTIONS {
        let mut res = StatusCode::NO_CONTENT.into_response();
        if let Some(origin_val) = origin.and_then(|o| HeaderValue::from_str(o).ok()) {
            res.headers_mut()
                .insert("access-control-allow-origin", origin_val);
            res.headers_mut()
                .insert("vary", HeaderValue::from_static("Origin"));
        }
        res.headers_mut().insert(
            "access-control-allow-methods",
            HeaderValue::from_static("GET, POST, OPTIONS"),
        );
        res.headers_mut().insert(
            "access-control-allow-headers",
            HeaderValue::from_static("content-type, authorization, x-ai-chat-memory-secret"),
        );
        return res;
    }

    let mut response = next.run(request).await;
    if let Some(origin_val) = origin.and_then(|o| HeaderValue::from_str(o).ok()) {
        response
            .headers_mut()
            .insert("access-control-allow-origin", origin_val);
        response
            .headers_mut()
            .insert("vary", HeaderValue::from_static("Origin"));
    }
    response.headers_mut().insert(
        "access-control-allow-headers",
        HeaderValue::from_static("content-type, authorization, x-ai-chat-memory-secret"),
    );
    response
}

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
        .layer(DefaultBodyLimit::max(20 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(
            app_service.clone(),
            authorize_mcp,
        ))
        .with_state(app_service)
}

async fn health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "ai-chat-memory-mcp",
    }))
}

#[cfg(test)]
mod tests {
    use super::{McpRejection, health, mcp_rejection_reason};
    use crate::local_services::{
        LocalServiceId, LocalServiceManager, LocalServiceSpec, LocalServiceStatus,
    };
    use crate::models::AppSettings;
    use axum::http::Method;
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

    fn mcp_settings(secret: Option<&str>) -> AppSettings {
        AppSettings {
            mcp_enabled: true,
            secret: secret.map(str::to_string),
            ..AppSettings::default()
        }
    }

    #[test]
    fn mcp_rejects_requests_without_origin_and_secret_even_when_secret_is_disabled() {
        // HTTP-2：secret_enabled=false 不能让无 Origin 的本机进程免密读取聊天记忆。
        let settings = mcp_settings(None);
        assert_eq!(
            mcp_rejection_reason(&settings, Method::GET, None, None),
            Some(McpRejection::MissingSecret)
        );
    }

    #[test]
    fn mcp_accepts_valid_secret_without_origin_and_rejects_wrong_secret() {
        let settings = mcp_settings(Some("s3cret"));
        assert_eq!(
            mcp_rejection_reason(&settings, Method::GET, None, Some("s3cret")),
            None
        );
        assert_eq!(
            mcp_rejection_reason(&settings, Method::GET, None, Some("wrong")),
            Some(McpRejection::MissingSecret)
        );
    }

    #[test]
    fn mcp_allows_whitelisted_preflight_without_secret_but_requires_it_for_real_requests() {
        let mut settings = mcp_settings(Some("s3cret"));
        settings.allowed_origins = vec!["https://chat.example.com".into()];
        assert_eq!(
            mcp_rejection_reason(
                &settings,
                Method::OPTIONS,
                Some("https://chat.example.com"),
                None
            ),
            None,
            "preflight cannot carry custom headers"
        );
        assert_eq!(
            mcp_rejection_reason(
                &settings,
                Method::GET,
                Some("https://chat.example.com"),
                None
            ),
            Some(McpRejection::MissingSecret),
            "HTTP-1：带 Origin 的浏览器请求无论 secret_enabled 都必须携带密钥"
        );
        assert_eq!(
            mcp_rejection_reason(
                &settings,
                Method::GET,
                Some("https://evil.example.com"),
                Some("s3cret")
            ),
            Some(McpRejection::OriginNotAllowed),
            "白名单外的 Origin 即使密钥正确也必须拒绝"
        );
    }

    #[test]
    fn mcp_rejects_every_request_when_disabled() {
        let settings = AppSettings::default();
        assert_eq!(
            mcp_rejection_reason(
                &settings,
                Method::GET,
                Some("https://chat.example.com"),
                Some("x")
            ),
            Some(McpRejection::Disabled)
        );
    }
}
