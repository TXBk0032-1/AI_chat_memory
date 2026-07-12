use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Query, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use tower_http::trace::TraceLayer;

use crate::{models::ImportRequest, service::AppService};

const CLIENT_HEADER: &str = "x-ai-chat-memory-client";
const CLIENT_VALUE: &str = "userscript-v1";
const SECRET_HEADER: &str = "x-ai-chat-memory-secret";

pub async fn serve(service: AppService) -> crate::error::Result<()> {
    let app = Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/sessions/import", post(import))
        .route("/api/v1/sessions/import/deepseek-export", post(import_zip))
        .route("/api/v1/sessions/sync-status", get(sync_status))
        .fallback(options)
        .layer(DefaultBodyLimit::max(128 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(service.clone(), authorize))
        .layer(TraceLayer::new_for_http())
        .with_state(service.clone());
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 19820))).await?;
    service
        .set_api_status(crate::models::ApiStatus::Running)
        .await;
    axum::serve(listener, app)
        .await
        .map_err(std::io::Error::other)?;
    Ok(())
}

async fn authorize(
    State(service): State<AppService>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    let settings = service.settings.get().await;
    if let Err(reason) = authorization_error(request.method(), origin, &headers, &settings) {
        return (StatusCode::FORBIDDEN, reason).into_response();
    }
    if request.method() != Method::OPTIONS {
        service.mark_userscript_request().await;
    }
    let mut response = if request.method() == Method::OPTIONS {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(request).await
    };
    if let Some(origin) = origin.and_then(|o| HeaderValue::from_str(o).ok()) {
        response
            .headers_mut()
            .insert("access-control-allow-origin", origin);
    }
    response
        .headers_mut()
        .insert("vary", HeaderValue::from_static("Origin"));
    response.headers_mut().insert(
        "access-control-allow-methods",
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    response.headers_mut().insert(
        "access-control-allow-headers",
        HeaderValue::from_static("content-type, x-ai-chat-memory-client, x-ai-chat-memory-secret"),
    );
    response
}

#[cfg(test)]
fn is_authorized(
    method: &Method,
    origin: Option<&str>,
    headers: &HeaderMap,
    settings: &crate::models::AppSettings,
) -> bool {
    authorization_error(method, origin, headers, settings).is_ok()
}

fn authorization_error(
    method: &Method,
    origin: Option<&str>,
    headers: &HeaderMap,
    settings: &crate::models::AppSettings,
) -> Result<(), &'static str> {
    let allowed =
        origin.is_some_and(|value| settings.allowed_origins.iter().any(|item| item == value));
    if !allowed {
        return Err("origin_not_allowed");
    }
    if *method == Method::OPTIONS {
        return Ok(());
    }
    let protocol_ok =
        headers.get(CLIENT_HEADER).and_then(|v| v.to_str().ok()) == Some(CLIENT_VALUE);
    if !protocol_ok {
        return Err("invalid_client");
    }
    let secret_ok = !settings.secret_enabled
        || headers.get(SECRET_HEADER).and_then(|v| v.to_str().ok()) == settings.secret.as_deref();
    if !secret_ok {
        return Err("invalid_secret");
    }
    Ok(())
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({"status":"ok","service":"ai-chat-memory"}))
}
async fn import(
    State(service): State<AppService>,
    Json(req): Json<ImportRequest>,
) -> impl IntoResponse {
    api_result(service.import(req).await)
}
async fn import_zip(State(service): State<AppService>, body: Bytes) -> impl IntoResponse {
    api_result(service.import_deepseek_zip(body.to_vec()).await)
}

#[derive(Deserialize)]
struct SyncQuery {
    platform: String,
}
async fn sync_status(
    State(service): State<AppService>,
    Query(query): Query<SyncQuery>,
) -> impl IntoResponse {
    match service.sync_status(&query.platform).await {
        Ok(value) => (StatusCode::OK, Json(json!({"last_updated_at":value}))).into_response(),
        Err(e) => error_response(e),
    }
}
async fn options() -> StatusCode {
    StatusCode::NO_CONTENT
}
fn api_result(result: crate::error::Result<crate::models::ImportResponse>) -> Response {
    match result {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => error_response(e),
    }
}
fn error_response(error: crate::error::AppError) -> Response {
    let status = match error {
        crate::error::AppError::InvalidData(_)
        | crate::error::AppError::Json(_)
        | crate::error::AppError::Zip(_) => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({"detail":error.to_string()}))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AppSettings;

    #[test]
    fn enforces_origin_protocol_and_optional_secret() {
        let mut settings = AppSettings::default();
        let mut headers = HeaderMap::new();
        headers.insert(CLIENT_HEADER, HeaderValue::from_static(CLIENT_VALUE));
        assert!(is_authorized(
            &Method::GET,
            Some("https://chat.deepseek.com"),
            &headers,
            &settings
        ));
        assert!(!is_authorized(
            &Method::GET,
            Some("https://evil.example"),
            &headers,
            &settings
        ));
        settings.secret_enabled = true;
        settings.secret = Some("secret".into());
        assert!(!is_authorized(
            &Method::GET,
            Some("https://chat.deepseek.com"),
            &headers,
            &settings
        ));
        assert_eq!(
            authorization_error(
                &Method::GET,
                Some("https://chat.deepseek.com"),
                &headers,
                &settings
            ),
            Err("invalid_secret")
        );
        headers.insert(SECRET_HEADER, HeaderValue::from_static("secret"));
        assert!(is_authorized(
            &Method::GET,
            Some("https://chat.deepseek.com"),
            &headers,
            &settings
        ));
        headers.remove(CLIENT_HEADER);
        assert!(is_authorized(
            &Method::OPTIONS,
            Some("https://chat.deepseek.com"),
            &headers,
            &settings
        ));
    }
}
