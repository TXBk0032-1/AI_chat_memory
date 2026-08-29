//! MCP server handlers (tools + resources). Wired by task 4+.
#![allow(dead_code)]

use crate::error::AppError;
use crate::mcp::params::{
    clamp_limit, clamp_offset, normalize_optional_search_date, normalize_required_query,
    normalize_required_session_id,
};
use crate::models::{SearchMode, SearchQuery};
use crate::service::AppService;
use rmcp::handler::server::{
    router::prompt::PromptRouter, router::tool::ToolRouter, wrapper::Parameters,
};
use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, prompt, prompt_handler, prompt_router,
    schemars, tool, tool_handler, tool_router,
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
    prompt_router: PromptRouter<Self>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchSessionsArgs {
    /// 关键词；省略则按更新时间列出
    #[serde(default)]
    q: Option<String>,
    /// 平台过滤，如 deepseek / doubao / kimi
    #[serde(default)]
    platform: Option<String>,
    /// 起始日期 YYYY-MM-DD（兼容 Unix 秒）
    #[serde(default)]
    date_from: Option<String>,
    /// 结束日期 YYYY-MM-DD（兼容 Unix 秒）
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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SummarizeSessionArgs {
    /// 会话 ID（可通过 search_sessions 工具或 sessions://recent 资源获取）
    session_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct FindMemoriesArgs {
    /// 主题词或要回忆的问题（必填）
    topic: String,
    /// 平台过滤，如 deepseek / doubao / kimi
    #[serde(default)]
    platform: Option<String>,
}

#[tool_router]
impl ChatMemoryMcp {
    pub fn new(service: AppService) -> Self {
        Self {
            service,
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
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
        let (date_from, date_to) =
            match normalize_search_dates(args.date_from.as_deref(), args.date_to.as_deref()) {
                Ok(value) => value,
                Err(msg) => return Ok(tool_error(msg)),
            };
        let mode = match parse_search_mode(args.mode.as_deref()) {
            Ok(m) => m,
            Err(msg) => return Ok(tool_error(msg)),
        };
        let query = SearchQuery {
            q: args.q,
            platform: args.platform,
            date_from,
            date_to,
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

#[prompt_router]
impl ChatMemoryMcp {
    #[prompt(
        name = "summarize-session",
        description = "生成总结指定会话的提示词：预取会话元数据与首窗消息，产出一段可直接发给模型的「总结此会话」用户消息。"
    )]
    async fn summarize_session(
        &self,
        Parameters(args): Parameters<SummarizeSessionArgs>,
    ) -> Result<GetPromptResult, McpError> {
        let session_id = normalize_required_session_id(&args.session_id)
            .map_err(|msg| McpError::invalid_params(msg, None))?;
        let open = self
            .service
            .open_session(&session_id, None)
            .await
            .map_err(|err| resource_mcp_error(&err))?;

        let mut text = format!(
            "请总结以下 AI 聊天会话。平台：{}，标题：{}。请按主题归纳双方要点、结论与待办事项，保留关键细节，用中文输出。\n\n",
            open.summary.platform, open.summary.title
        );
        if open.message_count > open.messages.len() {
            let next_seq = open
                .messages
                .last()
                .map(|m| m.seq + 1)
                .unwrap_or(open.start_seq);
            text.push_str(&format!(
                "以下为该会话首窗消息（第 {} 条起，共 {} 条）。如需更多上下文，可使用 get_messages 工具从 start_seq={} 继续读取。\n\n",
                open.start_seq,
                open.message_count,
                next_seq
            ));
        } else {
            text.push_str("以下为该会话全部消息。\n\n");
        }
        text.push_str("--- 会话记录 ---\n");
        for message in &open.messages {
            text.push_str(&format!("[{}] {}\n", message.role, message.content.trim()));
        }

        Ok(
            GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)])
                .with_description(format!("总结会话「{}」", open.summary.title)),
        )
    }

    #[prompt(
        name = "find-memories",
        description = "生成跨会话记忆检索的提示词：输入主题词，产出一段指导模型调用 search_sessions 等工具跨会话检索并归纳相关聊天记录的用户消息。"
    )]
    async fn find_memories(
        &self,
        Parameters(args): Parameters<FindMemoriesArgs>,
    ) -> Result<GetPromptResult, McpError> {
        let topic = normalize_required_query(&args.topic)
            .map_err(|msg| McpError::invalid_params(msg, None))?;
        let mut text = format!(
            "请在本地 AI 聊天记忆库中检索与「{topic}」相关的聊天记录，并跨会话归纳。步骤：\n\
             1. 使用 search_sessions 工具搜索关键词「{topic}」，必要时调整 limit/offset 分批获取；\n"
        );
        if let Some(platform) = args
            .platform
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            text.push_str(&format!("   - 仅限定平台「{platform}」；\n"));
        }
        text.push_str(
            "2. 用 open_session 打开相关会话，读取首窗消息确认相关性；\n\
             3. 归纳各会话中与该主题相关的观点、结论、时间线与分歧，并标注来源会话标题；\n\
             4. 最后给出跨会话的综合结论与尚不确定的部分。请用中文输出。",
        );

        Ok(
            GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)])
                .with_description(format!("跨会话检索「{topic}」")),
        )
    }
}

#[prompt_handler]
#[tool_handler]
impl ServerHandler for ChatMemoryMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_prompts()
                .enable_resources()
                .enable_tools()
                .build(),
        )
        .with_server_info(Implementation::new(
            "ai-chat-memory",
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(
            "只读本地 AI 聊天记忆库。需桌面应用在线。端点 http://127.0.0.1:19821/mcp。可用 Tools 搜索/打开/读消息；Resources：sessions://recent、session://{id}、session://{id}/messages；Prompts：summarize-session（总结指定会话）、find-memories（跨会话检索记忆）。"
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
        AppError::Crypto(msg) => format!("数据错误：{msg}"),
        AppError::Credential(msg) => format!("配置错误：{msg}"),
        AppError::Cloud(error) => format!("云同步错误：{error}"),
        AppError::SyncProtocol(msg) => format!("云同步协议错误：{msg}"),
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

fn normalize_search_dates(
    date_from: Option<&str>,
    date_to: Option<&str>,
) -> Result<(Option<String>, Option<String>), String> {
    Ok((
        normalize_optional_search_date(date_from, false)?,
        normalize_optional_search_date(date_to, true)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::test_support::test_app_service;
    use chrono::{Local, TimeZone, Timelike};
    use rmcp::ServiceExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    fn prompt_text(result: &GetPromptResult) -> &str {
        match &result.messages[0].content {
            ContentBlock::Text(text) => text.text.as_str(),
            other => panic!("expected text content, got {other:?}"),
        }
    }

    #[test]
    fn prompt_router_lists_two_prompts() {
        let names: Vec<String> = ChatMemoryMcp::prompt_router()
            .list_all()
            .into_iter()
            .map(|prompt| prompt.name.to_string())
            .collect();
        assert_eq!(
            names,
            vec!["find-memories".to_string(), "summarize-session".to_string()]
        );
    }

    #[tokio::test]
    async fn summarize_session_prompt_prefetches_first_window() {
        let (service, _data_dir) = test_app_service().await;
        let handler = ChatMemoryMcp::new(service);
        let result = handler
            .summarize_session(Parameters(SummarizeSessionArgs {
                session_id: "seed-session".into(),
            }))
            .await
            .unwrap();
        let text = prompt_text(&result);
        assert!(text.contains("deepseek"), "{text}");
        assert!(text.contains("Rust 异步编程讨论"), "{text}");
        assert!(
            text.contains("[user] 如何理解 Rust 的 async/await？"),
            "{text}"
        );
        assert!(
            text.contains("[assistant] async/await 是零开销抽象。"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn summarize_session_prompt_rejects_empty_and_unknown_session() {
        let (service, _data_dir) = test_app_service().await;
        let handler = ChatMemoryMcp::new(service);

        let err = handler
            .summarize_session(Parameters(SummarizeSessionArgs {
                session_id: "   ".into(),
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        assert!(err.message.contains("会话 ID 不能为空"), "{}", err.message);

        let err = handler
            .summarize_session(Parameters(SummarizeSessionArgs {
                session_id: "missing".into(),
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::RESOURCE_NOT_FOUND);
        assert!(err.message.contains("未找到"), "{}", err.message);
    }

    #[tokio::test]
    async fn find_memories_prompt_builds_retrieval_instructions() {
        let (service, _data_dir) = test_app_service().await;
        let handler = ChatMemoryMcp::new(service);

        let result = handler
            .find_memories(Parameters(FindMemoriesArgs {
                topic: "  量化交易  ".into(),
                platform: Some("deepseek".into()),
            }))
            .await
            .unwrap();
        let text = prompt_text(&result);
        assert!(text.contains("量化交易"), "{text}");
        assert!(text.contains("deepseek"), "{text}");
        assert!(text.contains("search_sessions"), "{text}");

        let err = handler
            .find_memories(Parameters(FindMemoriesArgs {
                topic: "   ".into(),
                platform: None,
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        assert!(err.message.contains("查询词不能为空"), "{}", err.message);
    }

    /// 协议级回归：stdio 与 duplex 共用 async_rw 传输（换行分隔 JSON-RPC），
    /// 此测试等价验证 stdio 传输下的 initialize → prompts → tools 全链路。
    #[tokio::test]
    async fn mcp_protocol_flow_over_memory_duplex() {
        let (service, _data_dir) = test_app_service().await;
        let (client_side, server_side) = tokio::io::duplex(64 * 1024);
        let (read_half, mut write_half) = tokio::io::split(client_side);

        // rmcp serve() 返回前会同步等待并处理客户端 initialize 握手，
        // 因此必须先把 initialize 写入 duplex 缓冲，再启动服务端。
        async fn write_message(
            writer: &mut tokio::io::WriteHalf<tokio::io::DuplexStream>,
            value: &serde_json::Value,
        ) {
            writer
                .write_all(format!("{value}\n").as_bytes())
                .await
                .unwrap();
        }
        async fn read_message(
            reader: &mut BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
        ) -> serde_json::Value {
            let mut line = Vec::new();
            reader.read_until(b'\n', &mut line).await.unwrap();
            serde_json::from_slice(&line).unwrap()
        }

        write_message(
            &mut write_half,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "mcp-test", "version": "0.0.0"}
                }
            }),
        )
        .await;

        let running = ChatMemoryMcp::new(service)
            .serve(server_side)
            .await
            .unwrap();
        let mut reader = BufReader::new(read_half);

        let response = read_message(&mut reader).await;
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["serverInfo"]["name"], "ai-chat-memory");
        assert!(response["result"]["capabilities"]["prompts"].is_object());
        assert!(response["result"]["capabilities"]["tools"].is_object());
        assert!(response["result"]["capabilities"]["resources"].is_object());

        write_message(
            &mut write_half,
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        .await;

        write_message(
            &mut write_half,
            &json!({"jsonrpc": "2.0", "id": 2, "method": "prompts/list"}),
        )
        .await;
        let response = read_message(&mut reader).await;
        assert_eq!(response["id"], 2);
        let names: Vec<&str> = response["result"]["prompts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|prompt| prompt["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"summarize-session"), "{names:?}");
        assert!(names.contains(&"find-memories"), "{names:?}");

        write_message(
            &mut write_half,
            &json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "prompts/get",
                "params": {
                    "name": "summarize-session",
                    "arguments": {"session_id": "seed-session"}
                }
            }),
        )
        .await;
        let response = read_message(&mut reader).await;
        assert_eq!(response["id"], 3);
        let text = response["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap();
        assert!(text.contains("Rust 异步编程讨论"), "{text}");

        write_message(
            &mut write_half,
            &json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "prompts/get",
                "params": {"name": "no-such-prompt", "arguments": {}}
            }),
        )
        .await;
        let response = read_message(&mut reader).await;
        assert_eq!(response["id"], 4);
        assert_eq!(response["error"]["code"], ErrorCode::INVALID_PARAMS.0);

        write_message(
            &mut write_half,
            &json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": {"name": "search_sessions", "arguments": {"q": "Rust"}}
            }),
        )
        .await;
        let response = read_message(&mut reader).await;
        assert_eq!(response["id"], 5);
        let tool_text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(tool_text.contains("seed-session"), "{tool_text}");

        let _ = running.cancel().await;
    }

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

    #[test]
    fn normalize_search_dates_wires_start_and_end_boundaries() {
        let (date_from, date_to) =
            normalize_search_dates(Some("2024-02-29"), Some("2024-02-29")).unwrap();
        let date_from = Local
            .timestamp_opt(date_from.unwrap().parse().unwrap(), 0)
            .single()
            .unwrap();
        let date_to = Local
            .timestamp_opt(date_to.unwrap().parse().unwrap(), 0)
            .single()
            .unwrap();

        assert_eq!(
            (date_from.hour(), date_from.minute(), date_from.second()),
            (0, 0, 0)
        );
        assert_eq!(
            (date_to.hour(), date_to.minute(), date_to.second()),
            (23, 59, 59)
        );
    }

    #[test]
    fn normalize_search_dates_rejects_invalid_date_to() {
        let err = normalize_search_dates(Some("2024-02-29"), Some("2024-02-30")).unwrap_err();
        assert!(err.contains("2024-02-30"), "{err}");
    }
}
