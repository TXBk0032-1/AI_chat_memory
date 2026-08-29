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
            HeaderValue::from_static("GET, POST, DELETE, OPTIONS"),
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
///
/// stateful 模式：initialize 下发 `Mcp-Session-Id`，GET 打开 SSE 长连接，
/// DELETE 终止会话。GET/DELETE 与 POST 一样受 secret + Origin fail-closed
/// 约束（见 `mcp_rejection_reason`），浏览器预检仍仅放行白名单 Origin。
pub fn build_mcp_router(app_service: AppService) -> Router {
    let service = StreamableHttpService::new(
        {
            let app_service = app_service.clone();
            move || Ok(ChatMemoryMcp::new(app_service.clone()))
        },
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_stateful_mode(true)
            .with_json_response(false),
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
    use super::{McpRejection, build_mcp_router, health, mcp_rejection_reason};
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
        // stateful SSE：DELETE 与 GET 长连接同样受 fail-closed 约束
        assert_eq!(
            mcp_rejection_reason(
                &AppSettings::default(),
                Method::DELETE,
                Some("https://chat.example.com"),
                Some("x")
            ),
            Some(McpRejection::Disabled)
        );
    }

    #[test]
    fn mcp_rejects_sse_get_and_delete_without_secret() {
        let settings = mcp_settings(None);
        for method in [Method::GET, Method::DELETE] {
            assert_eq!(
                mcp_rejection_reason(&settings, method.clone(), None, None),
                Some(McpRejection::MissingSecret),
                "{method} must require secret"
            );
            assert_eq!(
                mcp_rejection_reason(&settings, method.clone(), None, Some("wrong")),
                Some(McpRejection::MissingSecret),
                "{method} must reject wrong secret"
            );
        }
    }

    #[test]
    fn mcp_rejects_sse_get_and_delete_from_non_whitelisted_origin() {
        let mut settings = mcp_settings(Some("s3cret"));
        settings.allowed_origins = vec!["https://chat.example.com".into()];
        for method in [Method::GET, Method::DELETE] {
            assert_eq!(
                mcp_rejection_reason(
                    &settings,
                    method.clone(),
                    Some("https://evil.example.com"),
                    Some("s3cret")
                ),
                Some(McpRejection::OriginNotAllowed),
                "{method} must reject non-whitelisted origin"
            );
            assert_eq!(
                mcp_rejection_reason(
                    &settings,
                    method.clone(),
                    Some("https://chat.example.com"),
                    Some("s3cret")
                ),
                None,
                "{method} with valid secret and whitelisted origin must pass"
            );
        }
    }

    /// stateful SSE 全生命周期：initialize 下发 session id → GET SSE 长连接 →
    /// 缺失/未知 session 的拒绝 → DELETE 终止 → 热停机重建后旧 id 失效。
    #[tokio::test]
    async fn mcp_stateful_sse_lifecycle_via_manager() {
        use crate::mcp::test_support::test_app_service;

        const SSE_TEST_PORT: u16 = 19992; // 避免与健康探测的 19991 并发冲突
        let (app_service, _data_dir) = test_app_service().await;
        let rotated = app_service.rotate_secret().await.unwrap();
        let secret = rotated.secret.expect("rotate_secret stores a secret");

        let manager = LocalServiceManager::new();
        let build_service = app_service.clone();
        manager
            .register(LocalServiceSpec {
                id: LocalServiceId::Mcp,
                bind: SocketAddr::from(([127, 0, 0, 1], SSE_TEST_PORT)),
                build: Arc::new(move || build_mcp_router(build_service.clone())),
            })
            .await;
        manager.apply_desired(LocalServiceId::Mcp, true).await;

        let base = format!("http://127.0.0.1:{SSE_TEST_PORT}");
        let client = reqwest::Client::new();
        const DUAL_ACCEPT: &str = "application/json, text/event-stream";
        const SSE_ACCEPT: &str = "text/event-stream";

        // initialize：POST 创建会话，响应头下发 Mcp-Session-Id，响应体为 SSE 流
        let init_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "mcp-test", "version": "0.0.0"}
            }
        });
        let response = client
            .post(format!("{base}/mcp"))
            .header("x-ai-chat-memory-secret", &secret)
            .header("accept", DUAL_ACCEPT)
            .header("content-type", "application/json")
            .body(init_body.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "initialize must succeed");
        let session_id = response
            .headers()
            .get("mcp-session-id")
            .expect("stateful initialize must issue mcp-session-id")
            .to_str()
            .unwrap()
            .to_string();
        let body = read_sse_until(response, "\"id\":1").await;
        assert!(body.contains("ai-chat-memory"), "{body}");

        // initialized 通知（带 session id）
        let notification = client
            .post(format!("{base}/mcp"))
            .header("x-ai-chat-memory-secret", &secret)
            .header("mcp-session-id", &session_id)
            .header("accept", DUAL_ACCEPT)
            .header("content-type", "application/json")
            .body(
                serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
                    .to_string(),
            )
            .send()
            .await
            .unwrap();
        assert!(notification.status().is_success());

        // GET 打开 SSE 长连接 → 200 + text/event-stream + 首个事件
        let sse = client
            .get(format!("{base}/mcp"))
            .header("x-ai-chat-memory-secret", &secret)
            .header("mcp-session-id", &session_id)
            .header("accept", SSE_ACCEPT)
            .send()
            .await
            .unwrap();
        assert_eq!(sse.status(), 200);
        let content_type = sse
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(
            content_type.starts_with("text/event-stream"),
            "unexpected content-type: {content_type}"
        );
        let mut stream = sse;
        let first_chunk = tokio::time::timeout(Duration::from_secs(5), stream.chunk())
            .await
            .expect("GET SSE must emit priming event promptly")
            .unwrap()
            .expect("GET SSE stream must not be empty");
        assert!(!first_chunk.is_empty());
        drop(stream);

        // 缺失 session id → 400；未知 session id → 404
        let missing_session = client
            .get(format!("{base}/mcp"))
            .header("x-ai-chat-memory-secret", &secret)
            .header("accept", SSE_ACCEPT)
            .send()
            .await
            .unwrap();
        assert_eq!(missing_session.status(), 400);
        let unknown_session = client
            .get(format!("{base}/mcp"))
            .header("x-ai-chat-memory-secret", &secret)
            .header("mcp-session-id", "does-not-exist")
            .header("accept", SSE_ACCEPT)
            .send()
            .await
            .unwrap();
        assert_eq!(unknown_session.status(), 404);

        // 断线重连：带 Last-Event-ID 的 resume 路径返回 200
        //（本服务当前不推送 server 主动通知，重放内容为空，仅验证链路不报错）
        let resumed = client
            .get(format!("{base}/mcp"))
            .header("x-ai-chat-memory-secret", &secret)
            .header("mcp-session-id", &session_id)
            .header("accept", SSE_ACCEPT)
            .header("last-event-id", "0")
            .send()
            .await
            .unwrap();
        assert_eq!(resumed.status(), 200);
        drop(resumed);

        // DELETE 终止会话 → 旧 id 一律 404
        let deleted = client
            .delete(format!("{base}/mcp"))
            .header("x-ai-chat-memory-secret", &secret)
            .header("mcp-session-id", &session_id)
            .send()
            .await
            .unwrap();
        assert!(
            deleted.status().is_success(),
            "DELETE must terminate session"
        );
        let after_delete = client
            .get(format!("{base}/mcp"))
            .header("x-ai-chat-memory-secret", &secret)
            .header("mcp-session-id", &session_id)
            .header("accept", SSE_ACCEPT)
            .send()
            .await
            .unwrap();
        assert_eq!(after_delete.status(), 404);

        // 热停机重建：LocalSessionManager 随路由重建，旧 session id 失效，
        // 客户端按规范以 404 为信号重走 initialize。
        manager.apply_desired(LocalServiceId::Mcp, false).await;
        manager.apply_desired(LocalServiceId::Mcp, true).await;
        let stale_after_restart = client
            .get(format!("{base}/mcp"))
            .header("x-ai-chat-memory-secret", &secret)
            .header("mcp-session-id", &session_id)
            .header("accept", SSE_ACCEPT)
            .send()
            .await
            .unwrap();
        assert_eq!(stale_after_restart.status(), 404);

        manager.apply_desired(LocalServiceId::Mcp, false).await;
    }

    /// 读取 SSE 流直到出现目标片段（限时）；POST 响应流在交付结果后保持打开，
    /// 因此不能整块 `.text()` 等待流结束。
    async fn read_sse_until(mut response: reqwest::Response, needle: &str) -> String {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut buffer = String::new();
        while !buffer.contains(needle) {
            let chunk = match tokio::time::timeout_at(deadline, response.chunk()).await {
                Ok(Ok(Some(chunk))) => chunk,
                _ => break,
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk));
        }
        buffer
    }
}
