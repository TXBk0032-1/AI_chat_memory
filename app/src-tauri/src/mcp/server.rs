//! MCP server handlers (tools + resources). Wired by task 4+.
#![allow(dead_code)]

use crate::error::AppError;
use crate::mcp::params::{clamp_limit, clamp_offset, normalize_required_query};
use crate::models::{SearchMode, SearchQuery};
use crate::service::AppService;
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;
use serde_json::json;

pub const MCP_PORT: u16 = 19821;
pub const MCP_URL: &str = "http://127.0.0.1:19821/mcp";

const JSON_MIME: &str = "application/json";

#[derive(Clone)]
pub struct ChatMemoryMcp {
    pub service: AppService,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchSessionsArgs {
    /// 关键词；省略则按更新时间列出
    #[serde(default)]
    q: Option<String>,
    /// 平台过滤，如 deepseek / doubao / kimi
    #[serde(default)]
    platform: Option<String>,
    /// 起始日期 YYYY-MM-DD
    #[serde(default)]
    date_from: Option<String>,
    /// 结束日期 YYYY-MM-DD
    #[serde(default)]
    date_to: Option<String>,
    /// 搜索模式：keyword | semantic | hybrid
    #[serde(default)]
    mode: Option<String>,
    /// 返回条数，默认 20，最大 100
    #[serde(default)]
    limit: Option<i64>,
    /// 偏移，默认 0
    #[serde(default)]
    offset: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct OpenSessionArgs {
    /// 会话 ID
    session_id: String,
    /// 锚点消息 seq，可选
    #[serde(default)]
    anchor_seq: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetMessagesArgs {
    /// 会话 ID
    session_id: String,
    /// 起始 seq，默认 0
    #[serde(default)]
    start_seq: Option<i64>,
    /// 条数，默认 50，最大 100
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchInSessionArgs {
    /// 会话 ID
    session_id: String,
    /// 会话内查询词（必填）
    query: String,
    /// 搜索模式：keyword | semantic | hybrid
    #[serde(default)]
    mode: Option<String>,
}

#[tool_router]
impl ChatMemoryMcp {
    pub fn new(service: AppService) -> Self {
        Self {
            service,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "跨会话搜索本地已同步的 AI 聊天记录。返回会话摘要列表（id、platform、title、时间等）及 total。limit 默认 20、上限 100；offset 默认 0。mode 可选 keyword/semantic/hybrid。"
    )]
    async fn search_sessions(
        &self,
        Parameters(args): Parameters<SearchSessionsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let limit = clamp_limit(args.limit, 20, 100);
        let offset = clamp_offset(args.offset);
        let mode = match parse_search_mode(args.mode.as_deref()) {
            Ok(m) => m,
            Err(msg) => return Ok(tool_error(msg)),
        };
        let query = SearchQuery {
            q: args.q,
            platform: args.platform,
            date_from: args.date_from,
            date_to: args.date_to,
            limit: Some(limit),
            offset: Some(offset),
            mode,
        };
        match self.service.list(query).await {
            Ok(list) => Ok(json_success(&list)),
            Err(err) => Ok(tool_error(format_app_error(&err))),
        }
    }

    #[tool(
        description = "打开指定会话：返回元数据（title、message_count、has_branches 等）与首窗消息。session 不存在时返回错误。"
    )]
    async fn open_session(
        &self,
        Parameters(args): Parameters<OpenSessionArgs>,
    ) -> Result<CallToolResult, McpError> {
        match self
            .service
            .open_session(&args.session_id, args.anchor_seq)
            .await
        {
            Ok(open) => Ok(json_success(&open)),
            Err(err) => Ok(tool_error(format_app_error(&err))),
        }
    }

    #[tool(
        description = "按 seq 分页读取会话消息。start_seq 默认 0；limit 默认 50、上限 100。返回 Message 数组（id、role、content、seq 等）。"
    )]
    async fn get_messages(
        &self,
        Parameters(args): Parameters<GetMessagesArgs>,
    ) -> Result<CallToolResult, McpError> {
        let start_seq = args.start_seq.unwrap_or(0).max(0);
        let limit = clamp_limit(args.limit, 50, 100);
        match self
            .service
            .session_messages(&args.session_id, start_seq, limit)
            .await
        {
            Ok(messages) => Ok(json_success(&messages)),
            Err(err) => Ok(tool_error(format_app_error(&err))),
        }
    }

    #[tool(
        description = "在单个会话内搜索命中位置。query 不能为空。返回 SessionSearchHit 列表（message_id、seq、field、snippet 等）。"
    )]
    async fn search_in_session(
        &self,
        Parameters(args): Parameters<SearchInSessionArgs>,
    ) -> Result<CallToolResult, McpError> {
        let query = match normalize_required_query(&args.query) {
            Ok(q) => q,
            Err(msg) => return Ok(tool_error(msg)),
        };
        let mode = match parse_search_mode(args.mode.as_deref()) {
            Ok(m) => m,
            Err(msg) => return Ok(tool_error(msg)),
        };
        match self
            .service
            .session_search_hits(&args.session_id, &query, mode)
            .await
        {
            Ok(hits) => Ok(json_success(&hits)),
            Err(err) => Ok(tool_error(format_app_error(&err))),
        }
    }
}

#[tool_handler]
impl ServerHandler for ChatMemoryMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_resources()
                .enable_tools()
                .build(),
        )
        .with_server_info(Implementation::new(
            "ai-chat-memory",
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(
            "只读本地 AI 聊天记忆库。需桌面应用在线。端点 http://127.0.0.1:19821/mcp，仅环回、无鉴权。可用 Tools 搜索/打开/读消息；Resources：sessions://recent、session://{id}、session://{id}/messages。"
                .to_string(),
        )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            resources: vec![
                Resource::new("sessions://recent", "recent-sessions")
                    .with_description("最近会话摘要列表（可带 ?limit=，默认 20，最大 50）")
                    .with_mime_type(JSON_MIME),
            ],
            ..Default::default()
        })
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult {
            resource_templates: vec![
                ResourceTemplate::new("session://{id}", "session")
                    .with_description("会话摘要 + message_count + has_branches")
                    .with_mime_type(JSON_MIME),
                ResourceTemplate::new("session://{id}/messages", "session-messages")
                    .with_description("分页消息；query：start_seq、limit")
                    .with_mime_type(JSON_MIME),
            ],
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let uri = request.uri.as_str();
        match parse_resource_uri(uri) {
            ResourceTarget::Recent { limit } => {
                let query = SearchQuery {
                    q: None,
                    platform: None,
                    date_from: None,
                    date_to: None,
                    limit: Some(limit),
                    offset: Some(0),
                    mode: None,
                };
                let list = self
                    .service
                    .list(query)
                    .await
                    .map_err(|err| resource_mcp_error(&err))?;
                Ok(json_resource(uri, &list))
            }
            ResourceTarget::Session { id } => {
                let open = self
                    .service
                    .open_session(&id, None)
                    .await
                    .map_err(|err| resource_mcp_error(&err))?;
                let body = json!({
                    "id": open.summary.id,
                    "platform": open.summary.platform,
                    "platform_session_id": open.summary.platform_session_id,
                    "title": open.summary.title,
                    "created_at": open.summary.created_at,
                    "updated_at": open.summary.updated_at,
                    "imported_at": open.summary.imported_at,
                    "message_count": open.message_count,
                    "has_branches": open.has_branches,
                });
                Ok(json_resource(uri, &body))
            }
            ResourceTarget::Messages {
                id,
                start_seq,
                limit,
            } => {
                let messages = self
                    .service
                    .session_messages(&id, start_seq, limit)
                    .await
                    .map_err(|err| resource_mcp_error(&err))?;
                Ok(json_resource(uri, &messages))
            }
            ResourceTarget::Unknown => Err(McpError::resource_not_found(
                "资源不存在",
                Some(json!({ "uri": uri })),
            )),
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum ResourceTarget {
    Recent {
        limit: i64,
    },
    Session {
        id: String,
    },
    Messages {
        id: String,
        start_seq: i64,
        limit: i64,
    },
    Unknown,
}

pub(crate) fn parse_resource_uri(uri: &str) -> ResourceTarget {
    let (path, query) = match uri.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (uri, None),
    };

    if path == "sessions://recent" {
        let limit = query_i64(query, "limit");
        return ResourceTarget::Recent {
            limit: clamp_limit(limit, 20, 50),
        };
    }

    if let Some(rest) = path.strip_prefix("session://") {
        if let Some(id) = rest.strip_suffix("/messages") {
            if !id.is_empty() && !id.contains('/') {
                let start_seq = query_i64(query, "start_seq").unwrap_or(0).max(0);
                let limit = clamp_limit(query_i64(query, "limit"), 50, 100);
                return ResourceTarget::Messages {
                    id: id.to_string(),
                    start_seq,
                    limit,
                };
            }
        } else if !rest.is_empty() && !rest.contains('/') {
            return ResourceTarget::Session {
                id: rest.to_string(),
            };
        }
    }

    ResourceTarget::Unknown
}

fn query_i64(query: Option<&str>, key: &str) -> Option<i64> {
    let query = query?;
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        let k = parts.next()?;
        let v = parts.next().unwrap_or("");
        if k == key {
            return v.parse().ok();
        }
    }
    None
}

fn json_success<T: serde::Serialize>(value: &T) -> CallToolResult {
    match serde_json::to_string(value) {
        Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
        Err(err) => tool_error(format!("序列化结果失败：{err}")),
    }
}

fn tool_error(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.into())])
}

fn json_resource<T: serde::Serialize>(uri: &str, value: &T) -> ReadResourceResult {
    let text = serde_json::to_string(value)
        .unwrap_or_else(|err| json!({ "error": format!("序列化失败：{err}") }).to_string());
    ReadResourceResult::new(vec![
        ResourceContents::text(text, uri.to_string()).with_mime_type(JSON_MIME),
    ])
}

fn format_app_error(err: &AppError) -> String {
    match err {
        AppError::NotFound(msg) => format!("未找到：{msg}"),
        AppError::InvalidData(msg) => format!("参数无效：{msg}"),
        AppError::Configuration(msg) => format!("配置错误：{msg}"),
        AppError::Cancelled(msg) => format!("已取消：{msg}"),
        AppError::Database(e) => format!("数据库错误：{e}"),
        AppError::Io(e) => format!("I/O 错误：{e}"),
        AppError::Json(e) => format!("JSON 错误：{e}"),
        AppError::Zip(e) => format!("ZIP 错误：{e}"),
    }
}

fn resource_mcp_error(err: &AppError) -> McpError {
    match err {
        AppError::NotFound(msg) => McpError::resource_not_found(format!("未找到：{msg}"), None),
        other => McpError::internal_error(format_app_error(other), None),
    }
}

pub(crate) fn parse_search_mode(raw: Option<&str>) -> Result<Option<SearchMode>, String> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    match raw.to_ascii_lowercase().as_str() {
        "keyword" => Ok(Some(SearchMode::Keyword)),
        "semantic" => Ok(Some(SearchMode::Semantic)),
        "hybrid" => Ok(Some(SearchMode::Hybrid)),
        other => Err(format!(
            "无效的搜索模式「{other}」，请使用 keyword、semantic 或 hybrid"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SearchMode;

    #[test]
    fn parse_resource_uri_recent_default_and_limit() {
        assert_eq!(
            parse_resource_uri("sessions://recent"),
            ResourceTarget::Recent { limit: 20 }
        );
        assert_eq!(
            parse_resource_uri("sessions://recent?limit=5"),
            ResourceTarget::Recent { limit: 5 }
        );
        assert_eq!(
            parse_resource_uri("sessions://recent?limit=999"),
            ResourceTarget::Recent { limit: 50 }
        );
    }

    #[test]
    fn parse_resource_uri_session_and_messages() {
        assert_eq!(
            parse_resource_uri("session://abc-123"),
            ResourceTarget::Session {
                id: "abc-123".into()
            }
        );
        assert_eq!(
            parse_resource_uri("session://abc-123/messages?start_seq=10&limit=3"),
            ResourceTarget::Messages {
                id: "abc-123".into(),
                start_seq: 10,
                limit: 3,
            }
        );
        assert_eq!(
            parse_resource_uri("session://abc-123/messages"),
            ResourceTarget::Messages {
                id: "abc-123".into(),
                start_seq: 0,
                limit: 50,
            }
        );
    }

    #[test]
    fn parse_resource_uri_unknown() {
        assert_eq!(parse_resource_uri("foo://bar"), ResourceTarget::Unknown);
        assert_eq!(
            parse_resource_uri("session://a/b/messages"),
            ResourceTarget::Unknown
        );
        assert_eq!(parse_resource_uri("session://"), ResourceTarget::Unknown);
    }

    #[test]
    fn parse_search_mode_accepts_known_and_none() {
        assert_eq!(parse_search_mode(None).unwrap(), None);
        assert_eq!(parse_search_mode(Some("")).unwrap(), None);
        assert_eq!(parse_search_mode(Some("  ")).unwrap(), None);
        assert_eq!(
            parse_search_mode(Some("Keyword")).unwrap(),
            Some(SearchMode::Keyword)
        );
        assert_eq!(
            parse_search_mode(Some("semantic")).unwrap(),
            Some(SearchMode::Semantic)
        );
        assert_eq!(
            parse_search_mode(Some("HYBRID")).unwrap(),
            Some(SearchMode::Hybrid)
        );
    }

    #[test]
    fn parse_search_mode_rejects_unknown() {
        let err = parse_search_mode(Some("fuzzy")).unwrap_err();
        assert!(err.contains("fuzzy"), "{err}");
        assert!(err.contains("keyword"), "{err}");
    }
}
