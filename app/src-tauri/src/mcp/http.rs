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
