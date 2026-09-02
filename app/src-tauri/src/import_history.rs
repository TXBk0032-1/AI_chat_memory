//! 多格式历史导入：格式嗅探分发与 Cherry Studio / Chatbox / Kelivo / Gemini 解析器。
//!
//! 所有格式统一产出 [`NormalizedSession`]（工具调用/思考段进 `metadata`，与
//! normalizer 的 `metadata.tool_calls` / `metadata.thinking` 约定一致），由
//! `service.import_history` 落库。解析器对字段名做多候选取值，容忍各来源
//! 版本间的列名与结构差异。

use serde_json::{Value, json};
use sqlx::{Column, Row, SqlitePool, sqlite::SqliteConnectOptions};
use std::{
    collections::HashMap,
    io::{Cursor, Read},
    path::Path,
};
use uuid::Uuid;
use zip::ZipArchive;

use crate::{
    error::{AppError, Result},
    models::{NormalizedMessage, NormalizedSession},
    normalizer,
};

pub(crate) const MAX_CONVERSATIONS_JSON_BYTES: u64 = 128 * 1024 * 1024;
pub(crate) const CONVERSATIONS_JSON_TOO_LARGE: &str = "conversations.json 解压后超过 128 MB 限制";

pub(crate) fn read_zip_entry_with_limit<R: Read>(reader: R, max_bytes: u64) -> Result<String> {
    let mut content = String::new();
    let mut limited = reader.take(max_bytes.saturating_add(1));
    limited.read_to_string(&mut content)?;
    if content.len() as u64 > max_bytes {
        return Err(AppError::InvalidData(CONVERSATIONS_JSON_TOO_LARGE.into()));
    }
    Ok(content)
}

/// 嗅探并解析出的导入结果；`format` 用于日志与统计。
pub struct ImportedArchive {
    pub format: &'static str,
    pub sessions: Vec<NormalizedSession>,
}

/// 按内容嗅探导入格式并解析：ZIP → JSON（对象/数组）→ HTML。
pub async fn parse_import_history(bytes: Vec<u8>) -> Result<ImportedArchive> {
    if bytes.starts_with(b"PK") {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
        return parse_zip(&mut archive).await;
    }
    let text = String::from_utf8_lossy(&bytes);
    let trimmed = text.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        let value: Value = serde_json::from_str(trimmed.trim())
            .map_err(|_| AppError::InvalidData("导入文件不是有效的 JSON".into()))?;
        return parse_json(value);
    }
    if looks_like_gemini_takeout(trimmed) {
        return parse_gemini_takeout(trimmed);
    }
    Err(AppError::InvalidData(
        "无法识别的导入文件格式（支持 DeepSeek / Cherry Studio / Chatbox / Kelivo 备份与 Gemini Takeout）".into(),
    ))
}

async fn parse_zip(archive: &mut ZipArchive<Cursor<Vec<u8>>>) -> Result<ImportedArchive> {
    let names: Vec<String> = archive.file_names().map(str::to_owned).collect();
    let has = |name: &str| names.iter().any(|candidate| candidate == name);
    if has("conversations.json") {
        return parse_deepseek_zip(archive);
    }
    if has("cherrystudio.sqlite") {
        return parse_cherry_zip(archive).await;
    }
    if has("manifest.json")
        && names
            .iter()
            .any(|name| name.starts_with("sessions/") && name.ends_with("/session.json"))
    {
        return parse_chatbox_zip(archive);
    }
    Err(AppError::InvalidData(
        "无法识别的 ZIP 导入格式：缺少 conversations.json / cherrystudio.sqlite / chatbox sessions"
            .into(),
    ))
}

fn parse_json(value: Value) -> Result<ImportedArchive> {
    match &value {
        // DeepSeek 导出 conversations.json 的内容：会话对象数组。
        Value::Array(_) => {
            let conversations = serde_json::from_value::<Vec<Value>>(value)?;
            let sessions = conversations
                .iter()
                .map(normalizer::normalize_deepseek_export)
                .collect::<Result<Vec<_>>>()?;
            Ok(ImportedArchive {
                format: "deepseek",
                sessions,
            })
        }
        Value::Object(entries) => {
            if entries
                .keys()
                .any(|key| key.starts_with("session:") || key.starts_with("sessionMeta:"))
            {
                return parse_chatbox_legacy(&value);
            }
            if value
                .get("conversations")
                .and_then(Value::as_array)
                .is_some()
                && value.get("messages").and_then(Value::as_array).is_some()
            {
                return Ok(ImportedArchive {
                    format: "kelivo",
                    sessions: parse_kelivo(&value)?,
                });
            }
            Err(AppError::InvalidData(
                "无法识别的 JSON 导入格式（支持 Chatbox / Kelivo 导出与 DeepSeek conversations.json）".into(),
            ))
        }
        _ => Err(AppError::InvalidData("无法识别的导入文件格式".into())),
    }
}

fn parse_deepseek_zip(archive: &mut ZipArchive<Cursor<Vec<u8>>>) -> Result<ImportedArchive> {
    let file = archive
        .by_name("conversations.json")
        .map_err(|_| AppError::InvalidData("ZIP 中缺少 conversations.json".into()))?;
    if file.size() > MAX_CONVERSATIONS_JSON_BYTES {
        return Err(AppError::InvalidData(CONVERSATIONS_JSON_TOO_LARGE.into()));
    }
    if file.compressed_size() > 0 && file.size() / file.compressed_size() > 200 {
        return Err(AppError::InvalidData("ZIP 压缩比异常".into()));
    }
    let content = read_zip_entry_with_limit(file, MAX_CONVERSATIONS_JSON_BYTES)?;
    let conversations: Vec<Value> = serde_json::from_str(&content)?;
    let sessions = conversations
        .iter()
        .map(normalizer::normalize_deepseek_export)
        .collect::<Result<Vec<_>>>()?;
    Ok(ImportedArchive {
        format: "deepseek",
        sessions,
    })
}

// ---------------------------------------------------------------------------
// Cherry Studio v2 备份：ZIP 内 metadata.json + cherrystudio.sqlite，
// topic/message 表，message.data JSON 的 parts[] 按 type 拆 content/thinking/tool_calls。
// ---------------------------------------------------------------------------

const MAX_SQLITE_ENTRY_BYTES: usize = 512 * 1024 * 1024;

async fn parse_cherry_zip(archive: &mut ZipArchive<Cursor<Vec<u8>>>) -> Result<ImportedArchive> {
    let mut sqlite_bytes = Vec::new();
    archive
        .by_name("cherrystudio.sqlite")?
        .read_to_end(&mut sqlite_bytes)?;
    if sqlite_bytes.len() > MAX_SQLITE_ENTRY_BYTES {
        return Err(AppError::InvalidData(
            "cherrystudio.sqlite 超过 512 MB 限制".into(),
        ));
    }
    let workdir = std::env::temp_dir().join(format!("acm-import-{}", Uuid::new_v4()));
    tokio::fs::create_dir_all(&workdir).await?;
    let db_path = workdir.join("cherrystudio.sqlite");
    let parse_result = async {
        tokio::fs::write(&db_path, &sqlite_bytes).await?;
        read_cherry_sessions(&db_path).await
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&workdir).await;
    parse_result
}

async fn read_cherry_sessions(db_path: &Path) -> Result<ImportedArchive> {
    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .read_only(true);
    let pool = sqlx::SqlitePool::connect_with(options).await?;
    let result = read_cherry_with_pool(&pool).await;
    pool.close().await;
    result
}

async fn read_cherry_with_pool(pool: &SqlitePool) -> Result<ImportedArchive> {
    let topics = table_rows(pool, &["topic", "topics"])
        .await?
        .ok_or_else(|| AppError::InvalidData("Cherry Studio 备份缺少 topic 表".into()))?;
    let messages = table_rows(pool, &["message", "messages"])
        .await?
        .ok_or_else(|| AppError::InvalidData("Cherry Studio 备份缺少 message 表".into()))?;
    Ok(ImportedArchive {
        format: "cherry",
        sessions: cherry_sessions(&topics, &messages),
    })
}

/// 读取候选表名为整表 JSON 行；表名来自固定白名单，不拼用户输入。
async fn table_rows(pool: &SqlitePool, candidates: &[&str]) -> Result<Option<Vec<Value>>> {
    for table in candidates {
        let exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        )
        .bind(table)
        .fetch_one(pool)
        .await?;
        if exists == 0 {
            continue;
        }
        let rows = sqlx::query(&format!("SELECT * FROM \"{table}\""))
            .fetch_all(pool)
            .await?;
        let mut mapped = Vec::with_capacity(rows.len());
        for row in rows {
            let mut object = serde_json::Map::new();
            for column in row.columns() {
                let name = column.name();
                let value = if let Ok(value) = row.try_get::<Option<String>, _>(name) {
                    value.map(Value::String).unwrap_or(Value::Null)
                } else if let Ok(value) = row.try_get::<Option<i64>, _>(name) {
                    value.map(|number| json!(number)).unwrap_or(Value::Null)
                } else if let Ok(value) = row.try_get::<Option<f64>, _>(name) {
                    value.map(|number| json!(number)).unwrap_or(Value::Null)
                } else if let Ok(value) = row.try_get::<Option<Vec<u8>>, _>(name) {
                    value
                        .map(|bytes| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
                        .unwrap_or(Value::Null)
                } else {
                    Value::Null
                };
                object.insert(name.to_owned(), value);
            }
            mapped.push(Value::Object(object));
        }
        return Ok(Some(mapped));
    }
    Ok(None)
}

fn cherry_sessions(topics: &[Value], messages: &[Value]) -> Vec<NormalizedSession> {
    let mut sessions = Vec::new();
    for (index, topic) in topics.iter().enumerate() {
        let topic_id = str_field(topic, &["id", "topic_id", "uuid"]).unwrap_or_default();
        let topic_messages: Vec<&Value> = messages
            .iter()
            .filter(|message| {
                str_field(message, &["topic_id", "topicId", "topic"]).as_deref()
                    == Some(topic_id.as_str())
            })
            .collect();
        let platform_session_id = if topic_id.is_empty() {
            // 兜底：内容哈希保证重复导入仍可去重。
            content_digest(topic)
        } else {
            topic_id
        };
        let raw_data = json!({
            "source": "cherry_backup",
            "topic": topic,
            "messages": topic_messages,
        });
        sessions.push(NormalizedSession {
            id: Uuid::new_v4().to_string(),
            platform: "cherry".into(),
            platform_session_id,
            title: str_field(topic, &["name", "title", "subject"]).unwrap_or_default(),
            created_at: timestamp_field(
                topic,
                &["created_at", "createdAt", "create_time", "createTime"],
            ),
            updated_at: timestamp_field(
                topic,
                &["updated_at", "updatedAt", "update_time", "updateTime"],
            ),
            imported_at: chrono::Utc::now().to_rfc3339(),
            messages: topic_messages
                .iter()
                .map(|message| cherry_message(message))
                .collect(),
            raw_data,
        });
        let _ = index;
    }
    sessions
}

fn cherry_message(message: &Value) -> NormalizedMessage {
    let role = str_field(message, &["role"]).unwrap_or_else(|| "user".to_owned());
    let data = message
        .get("data")
        .and_then(|value| match value {
            Value::String(text) => serde_json::from_str::<Value>(text).ok(),
            object @ Value::Object(_) => Some(object.clone()),
            _ => None,
        })
        .unwrap_or(Value::Null);

    let mut content = Vec::new();
    let mut thinking = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    if let Some(parts) = data.get("parts").and_then(Value::as_array) {
        for part in parts {
            match str_field(part, &["type"]).as_deref() {
                Some("thinking") | Some("reasoning") => {
                    if let Some(text) = str_field(part, &["content", "text"]) {
                        thinking.push(text);
                    }
                }
                Some("tool_calls") | Some("tool_use") | Some("tool") => {
                    collect_tool_call_values(part, &mut tool_calls);
                }
                // 文本分段与未知类型尽量按文本保底。
                _ => {
                    if let Some(text) = str_field(part, &["content", "text"]) {
                        content.push(text);
                    }
                }
            }
        }
    }
    if content.is_empty()
        && let Some(text) = data.get("content").and_then(Value::as_str)
    {
        content.push(text.to_owned());
    }
    if thinking.is_empty() {
        for key in ["reasoning_content", "reasoningContent", "thinking"] {
            if let Some(text) = data.get(key).and_then(Value::as_str) {
                thinking.push(text.to_owned());
                break;
            }
        }
    }
    if tool_calls.is_empty()
        && let Some(calls) = data.get("tool_calls").and_then(Value::as_array)
    {
        calls
            .iter()
            .for_each(|call| push_tool_call(call, &mut tool_calls));
    }

    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "model".into(),
        data.get("model").cloned().unwrap_or(Value::Null),
    );
    metadata.insert(
        "thinking".into(),
        if thinking.is_empty() {
            Value::Null
        } else {
            Value::String(thinking.join("\n"))
        },
    );
    if !tool_calls.is_empty() {
        metadata.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    NormalizedMessage {
        role,
        content: content.join("\n"),
        metadata: Value::Object(metadata),
        created_at: timestamp_field(
            message,
            &[
                "created_at",
                "createdAt",
                "create_time",
                "createTime",
                "time",
            ],
        ),
    }
}

// ---------------------------------------------------------------------------
// Chatbox：新备份 ZIP（manifest.json format=chatbox-backup + sessions/*/session.json）
// 与旧版扁平 localStorage JSON（键 session:<id>）。
// ---------------------------------------------------------------------------

fn parse_chatbox_zip(archive: &mut ZipArchive<Cursor<Vec<u8>>>) -> Result<ImportedArchive> {
    let manifest = {
        let file = archive
            .by_name("manifest.json")
            .map_err(|_| AppError::InvalidData("ZIP 中缺少 manifest.json".into()))?;
        let content = read_zip_entry_with_limit(file, MAX_CONVERSATIONS_JSON_BYTES)?;
        serde_json::from_str::<Value>(&content)?
    };
    let format = str_field(&manifest, &["format"]).unwrap_or_default();
    if !format.contains("chatbox") {
        return Err(AppError::InvalidData(
            "manifest.json 不是 chatbox-backup 备份".into(),
        ));
    }
    let session_paths: Vec<String> = archive
        .file_names()
        .filter(|name| name.starts_with("sessions/") && name.ends_with("/session.json"))
        .map(str::to_owned)
        .collect();
    let mut sessions = Vec::new();
    for path in session_paths {
        let file = archive.by_name(&path)?;
        let content = read_zip_entry_with_limit(file, MAX_CONVERSATIONS_JSON_BYTES)?;
        let value: Value = serde_json::from_str(&content)?;
        let directory_id = path
            .trim_start_matches("sessions/")
            .trim_end_matches("/session.json")
            .to_owned();
        sessions.push(chatbox_session(
            &value,
            &directory_id,
            json!({"source": "chatbox_backup", "entry": path}),
        ));
    }
    Ok(ImportedArchive {
        format: "chatbox",
        sessions,
    })
}

fn parse_chatbox_legacy(value: &Value) -> Result<ImportedArchive> {
    let entries = value
        .as_object()
        .ok_or_else(|| AppError::InvalidData("Chatbox 导出不是 JSON 对象".into()))?;
    let mut sessions = Vec::new();
    for (key, session_value) in entries {
        let Some(session_id) = key.strip_prefix("session:") else {
            continue;
        };
        sessions.push(chatbox_session(
            session_value,
            session_id,
            json!({"source": "chatbox_legacy", "key": key}),
        ));
    }
    if sessions.is_empty() {
        return Err(AppError::InvalidData(
            "JSON 中没有 session:<id> 键，不是 Chatbox 旧版导出".into(),
        ));
    }
    Ok(ImportedArchive {
        format: "chatbox",
        sessions,
    })
}

fn chatbox_session(value: &Value, fallback_id: &str, raw_data: Value) -> NormalizedSession {
    let session_id = str_field(value, &["id"])
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| fallback_id.to_owned());
    NormalizedSession {
        id: Uuid::new_v4().to_string(),
        platform: "chatbox".into(),
        platform_session_id: session_id,
        title: str_field(value, &["name", "title"]).unwrap_or_default(),
        created_at: timestamp_field(value, &["createdAt", "created_at", "created"]),
        updated_at: timestamp_field(value, &["updatedAt", "updated_at", "updated"]),
        imported_at: chrono::Utc::now().to_rfc3339(),
        messages: value
            .get("messages")
            .and_then(Value::as_array)
            .map(|messages| messages.iter().map(chatbox_message).collect())
            .unwrap_or_default(),
        raw_data,
    }
}

fn chatbox_message(message: &Value) -> NormalizedMessage {
    let role = str_field(message, &["role"]).unwrap_or_else(|| "user".to_owned());
    let mut content = Vec::new();
    let mut thinking = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    if let Some(parts) = message.get("contentParts").and_then(Value::as_array) {
        for part in parts {
            match str_field(part, &["type"]).as_deref() {
                Some("thinking") | Some("reasoning") | Some("reasoning_content") => {
                    if let Some(text) = str_field(part, &["text", "content"]) {
                        thinking.push(text);
                    }
                }
                Some("tool_use") | Some("tool_calls") | Some("tool") => {
                    collect_tool_call_values(part, &mut tool_calls);
                }
                // 文本与未识别分段（image_url 等媒体只有 URL 文本可保留）。
                _ => {
                    if let Some(text) = str_field(part, &["text", "content"]) {
                        content.push(text);
                    }
                }
            }
        }
    }
    if content.is_empty()
        && let Some(text) = message.get("content").and_then(Value::as_str)
    {
        content.push(text.to_owned());
    }
    if thinking.is_empty() {
        for key in ["reasoning_content", "reasoningContent"] {
            if let Some(text) = message.get(key).and_then(Value::as_str) {
                thinking.push(text.to_owned());
                break;
            }
        }
    }
    if tool_calls.is_empty()
        && let Some(calls) = message.get("tool_calls").and_then(Value::as_array)
    {
        calls
            .iter()
            .for_each(|call| push_tool_call(call, &mut tool_calls));
    }

    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "model".into(),
        message.get("model").cloned().unwrap_or(Value::Null),
    );
    metadata.insert(
        "thinking".into(),
        if thinking.is_empty() {
            Value::Null
        } else {
            Value::String(thinking.join("\n"))
        },
    );
    if !tool_calls.is_empty() {
        metadata.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    NormalizedMessage {
        role,
        content: content.join("\n"),
        metadata: Value::Object(metadata),
        created_at: timestamp_field(message, &["createdAt", "created_at", "timestamp"]),
    }
}

// ---------------------------------------------------------------------------
// Kelivo：chats.json（version:1, conversations[] + messages[] + toolEvents[]），
// message.parts 分段 text/reasoning/tool_call，reasoningText 兜底字段。
// ---------------------------------------------------------------------------

fn parse_kelivo(value: &Value) -> Result<Vec<NormalizedSession>> {
    let conversations = value
        .get("conversations")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::InvalidData("Kelivo 导出缺少 conversations 数组".into()))?;
    let messages = value
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let tool_events = value
        .get("toolEvents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let events_by_id: HashMap<String, &Value> = tool_events
        .iter()
        .filter_map(|event| str_field(event, &["id"]).map(|id| (id, event)))
        .collect();
    let mut sessions = Vec::new();
    for conversation in conversations {
        let conversation_id = str_field(conversation, &["id"]).unwrap_or_default();
        let related: Vec<&Value> = messages
            .iter()
            .filter(|message| {
                str_field(message, &["conversationId", "conversation_id", "convId"]).as_deref()
                    == Some(conversation_id.as_str())
            })
            .collect();
        let session_id = if conversation_id.is_empty() {
            content_digest(conversation)
        } else {
            conversation_id
        };
        let raw_data = json!({
            "source": "kelivo_chats",
            "conversation": conversation,
            "messages": related,
        });
        sessions.push(NormalizedSession {
            id: Uuid::new_v4().to_string(),
            platform: "kelivo".into(),
            platform_session_id: session_id,
            title: str_field(conversation, &["title", "name"]).unwrap_or_default(),
            created_at: timestamp_field(conversation, &["createdAt", "created_at", "createTime"]),
            updated_at: timestamp_field(conversation, &["updatedAt", "updated_at", "updateTime"]),
            imported_at: chrono::Utc::now().to_rfc3339(),
            messages: related
                .iter()
                .map(|message| kelivo_message(message, &events_by_id))
                .collect(),
            raw_data,
        });
    }
    Ok(sessions)
}

fn kelivo_message(message: &Value, events_by_id: &HashMap<String, &Value>) -> NormalizedMessage {
    let role = str_field(message, &["role"]).unwrap_or_else(|| "user".to_owned());
    let mut content = Vec::new();
    let mut thinking = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    if let Some(parts) = message.get("parts").and_then(Value::as_array) {
        for part in parts {
            match str_field(part, &["type"]).as_deref() {
                Some("text") => {
                    if let Some(text) = str_field(part, &["text", "content"]) {
                        content.push(text);
                    }
                }
                Some("reasoning") => {
                    if let Some(text) = str_field(part, &["text", "content"]) {
                        thinking.push(text);
                    }
                }
                Some("tool_call") => {
                    // 分段引用 toolEvents：按 toolCallId/id 关联，缺失时也保底记录分段内容。
                    let event_id = str_field(part, &["toolCallId", "id", "callId", "eventId"]);
                    match event_id.as_deref().and_then(|id| events_by_id.get(id)) {
                        Some(event) => push_tool_call(event, &mut tool_calls),
                        None => collect_tool_call_values(part, &mut tool_calls),
                    }
                }
                _ => {
                    if let Some(text) = str_field(part, &["text", "content"]) {
                        content.push(text);
                    }
                }
            }
        }
    }
    if content.is_empty()
        && let Some(text) = message.get("content").and_then(Value::as_str)
    {
        content.push(text.to_owned());
    }
    if thinking.is_empty()
        && let Some(text) = message.get("reasoningText").and_then(Value::as_str)
    {
        thinking.push(text.to_owned());
    }
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "model".into(),
        message.get("model").cloned().unwrap_or(Value::Null),
    );
    metadata.insert(
        "thinking".into(),
        if thinking.is_empty() {
            Value::Null
        } else {
            Value::String(thinking.join("\n"))
        },
    );
    if !tool_calls.is_empty() {
        metadata.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    NormalizedMessage {
        role,
        content: content.join("\n"),
        metadata: Value::Object(metadata),
        created_at: timestamp_field(message, &["createdAt", "created_at", "createTime"]),
    }
}

// ---------------------------------------------------------------------------
// Gemini Takeout：My Activity/Gemini Apps/MyActivity.html，outer-cell 条目
// → Prompt/Response 对；无会话边界，每条活动一个会话。
// ---------------------------------------------------------------------------

fn looks_like_gemini_takeout(text: &str) -> bool {
    text.contains("outer-cell") && (text.contains("Gemini Apps") || text.contains("My Activity"))
}

fn parse_gemini_takeout(html: &str) -> Result<ImportedArchive> {
    let mut sessions = Vec::new();
    for chunk in html.split("outer-cell").skip(1) {
        let cells = extract_div_blocks(chunk, "content-cell");
        if cells.is_empty() {
            continue;
        }
        let prompt_text = html_to_text(cells[0]);
        let prompt_lines = prompt_text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty());
        let mut prompt_segments: Vec<String> = prompt_lines.map(str::to_owned).collect();
        // 单元格末行是活动时间（形如 "2026年8月30日 12:34:56 GMT+8"），
        // 用于会话去重指纹，不并入正文。
        let activity_time = prompt_segments
            .last()
            .filter(|line| is_activity_time_line(line))
            .cloned();
        if activity_time.is_some() {
            prompt_segments.pop();
        }
        if let Some(first) = prompt_segments.first_mut() {
            // Takeout 在提示语前带动作前缀（英文 "Prompted "、中文 "已提示"），剥离。
            if let Some(stripped) = first
                .strip_prefix("Prompted ")
                .or_else(|| first.strip_prefix("已提示"))
            {
                *first = stripped.trim_start().to_owned();
            }
        }
        let prompt = prompt_segments.join("\n");
        let response = cells[1..]
            .iter()
            .map(|cell| {
                html_to_text(cell)
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if prompt.is_empty() && response.is_empty() {
            continue;
        }
        let digest = sha256_hex(&format!(
            "{}|{}",
            activity_time.as_deref().unwrap_or(""),
            prompt
        ));
        let mut messages = Vec::new();
        if !prompt.is_empty() {
            messages.push(gemini_message("user", &prompt));
        }
        if !response.is_empty() {
            messages.push(gemini_message("assistant", &response));
        }
        if messages.is_empty() {
            continue;
        }
        let title = prompt
            .lines()
            .next()
            .unwrap_or("Gemini 活动")
            .chars()
            .take(60)
            .collect::<String>();
        sessions.push(NormalizedSession {
            id: Uuid::new_v4().to_string(),
            platform: "gemini".into(),
            platform_session_id: digest,
            title,
            created_at: None,
            updated_at: None,
            imported_at: chrono::Utc::now().to_rfc3339(),
            messages,
            raw_data: json!({
                "source": "gemini_takeout",
                "activity_time": activity_time,
                "html": chunk.trim().chars().take(64 * 1024).collect::<String>(),
            }),
        });
    }
    if sessions.is_empty() {
        return Err(AppError::InvalidData(
            "Gemini Takeout HTML 中没有可解析的活动条目".into(),
        ));
    }
    Ok(ImportedArchive {
        format: "gemini",
        sessions,
    })
}

fn gemini_message(role: &str, text: &str) -> NormalizedMessage {
    NormalizedMessage {
        role: role.into(),
        content: text.to_owned(),
        metadata: json!({}),
        created_at: None,
    }
}

fn is_activity_time_line(line: &str) -> bool {
    line.contains("GMT")
        || line.contains("UTC")
        || (line.contains("20") && line.contains("日") && line.contains(':'))
        || (line.contains("年") && line.contains("月") && line.contains("日"))
}

/// 提取包含 `class_token` 的 div 块内部 HTML（跟踪 div 嵌套深度）。
fn extract_div_blocks<'a>(fragment: &'a str, class_token: &str) -> Vec<&'a str> {
    let mut blocks = Vec::new();
    let mut cursor = fragment;
    while let Some(class_position) = cursor.find(class_token) {
        let Some(open_start) = cursor[..class_position].rfind("<div") else {
            break;
        };
        let Some(open_end) = cursor[open_start..].find('>') else {
            break;
        };
        let mut scan = open_start + open_end + 1;
        let mut depth = 1usize;
        let mut body_end = None;
        loop {
            let rest = &cursor[scan..];
            let Some(close_rel) = rest.find("</div") else {
                break;
            };
            // "</div" 不会包含 "<div" 子串，比较两者位置即可判定先后。
            let open_rel = rest.find("<div");
            match open_rel {
                Some(open) if open < close_rel => {
                    depth += 1;
                    scan += open + 4;
                }
                _ => {
                    depth -= 1;
                    if depth == 0 {
                        body_end = Some(scan + close_rel);
                        break;
                    }
                    scan += close_rel + 5;
                }
            }
            if scan >= cursor.len() {
                break;
            }
        }
        match body_end {
            Some(end) => {
                let body_start = open_start + open_end + 1;
                blocks.push(&cursor[body_start..end]);
                cursor = &cursor[(end + 6).min(cursor.len())..];
            }
            None => break,
        }
    }
    blocks
}

/// 剥离 HTML 标签并解码常见实体；br/p 折行为换行。
fn html_to_text(fragment: &str) -> String {
    let mut text = String::new();
    let mut rest = fragment;
    while let Some(position) = rest.find('<') {
        text.push_str(&decode_entities(&rest[..position]));
        let tag = &rest[position..];
        let Some(length) = tag.find('>') else {
            text.push_str(&decode_entities(tag));
            return text;
        };
        let name = tag[1..length].to_ascii_lowercase();
        if name == "br"
            || name == "br/"
            || name.starts_with("br ")
            || name == "p"
            || name.starts_with("p ")
            || name == "/p"
        {
            text.push('\n');
        }
        rest = &rest[position + length + 1..];
    }
    text.push_str(&decode_entities(rest));
    text
}

fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(position) = rest.find('&') {
        output.push_str(&rest[..position]);
        let tail = &rest[position..];
        let Some(length) = tail.find(';') else {
            output.push_str(tail);
            return output;
        };
        let entity = &tail[1..length];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            numeric if numeric.starts_with('#') => {
                let code = numeric[1..]
                    .strip_prefix('x')
                    .or_else(|| numeric[1..].strip_prefix('X'))
                    .map(|digits| u32::from_str_radix(digits, 16).ok())
                    .unwrap_or_else(|| numeric[1..].parse::<u32>().ok());
                code.and_then(char::from_u32)
            }
            _ => None,
        };
        match decoded {
            Some(character) => output.push(character),
            None => output.push_str(&tail[..=length]),
        }
        rest = &rest[position + length + 1..];
    }
    output.push_str(rest);
    output
}

// ---------------------------------------------------------------------------
// 共享工具：字段候选取值、工具调用归一、内容指纹。
// ---------------------------------------------------------------------------

fn str_field(value: &Value, candidates: &[&str]) -> Option<String> {
    candidates.iter().find_map(|key| {
        value.get(*key).and_then(|field| match field {
            Value::String(text) => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
    })
}

fn timestamp_field(value: &Value, candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .find_map(|key| normalizer::normalize_timestamp(value.get(*key)))
}

/// 归一为 `metadata.tool_calls` 约定条目：{name, args?, result?, status?, results_count?}。
fn push_tool_call(entry: &Value, out: &mut Vec<Value>) {
    let Some(object) = entry.as_object() else {
        return;
    };
    let name = ["name", "toolName", "tool_name", "function"]
        .iter()
        .find_map(|key| {
            let value = object.get(*key)?;
            match value {
                Value::String(text) => Some(text.clone()),
                Value::Object(function) => function
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                _ => None,
            }
        });
    let Some(name) = name.filter(|name| !name.is_empty()) else {
        return;
    };
    let mut call = serde_json::Map::new();
    call.insert("name".into(), Value::String(name));
    for (key, target) in [("args", "args"), ("arguments", "args"), ("input", "args")] {
        if let Some(args) = object.get(key)
            && !args.is_null()
        {
            call.insert(target.into(), args.clone());
            break;
        }
    }
    for (key, target) in [
        ("result", "result"),
        ("output", "result"),
        ("content", "result"),
    ] {
        if let Some(result) = object.get(key)
            && !result.is_null()
        {
            call.insert(target.into(), result.clone());
            break;
        }
    }
    if let Some(status) = object.get("status").and_then(Value::as_str) {
        call.insert("status".into(), Value::String(status.to_owned()));
    }
    if let Some(count) = object.get("results_count").and_then(Value::as_u64) {
        call.insert("results_count".into(), json!(count));
    }
    out.push(Value::Object(call));
}

fn collect_tool_call_values(part: &Value, out: &mut Vec<Value>) {
    // 分段自身携带 tool_calls 数组，或 content 字段是 JSON 数组文本。
    if let Some(calls) = part.get("tool_calls").and_then(Value::as_array) {
        calls.iter().for_each(|call| push_tool_call(call, out));
        return;
    }
    if let Some(text) = part.get("content").and_then(Value::as_str)
        && let Ok(Value::Array(calls)) = serde_json::from_str::<Value>(text)
    {
        calls.iter().for_each(|call| push_tool_call(call, out));
    }
}

fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(text.as_bytes());
    hex::encode(digest)
}

fn content_digest(value: &Value) -> String {
    sha256_hex(&value.to_string())[..32].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    fn build_zip(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut writer = ZipWriter::new(Cursor::new(&mut bytes));
            for (name, content) in entries {
                writer
                    .start_file(
                        name.as_str(),
                        SimpleFileOptions::default()
                            .compression_method(CompressionMethod::Deflated),
                    )
                    .unwrap();
                writer.write_all(content).unwrap();
            }
            writer.finish().unwrap();
        }
        bytes
    }

    fn text_entry(name: &str, content: &str) -> (String, Vec<u8>) {
        (name.to_owned(), content.as_bytes().to_vec())
    }

    #[tokio::test]
    async fn sniffs_deepseek_zip_and_normalizes_conversations() {
        let conversations = r#"[{"id":"conv-1","title":"T","inserted_at":1756000000,
            "mapping":{
                "n1":{"id":"n1","message":{"fragments":[{"type":"REQUEST","content":"q"}]}},
                "n2":{"id":"n2","parent":"n1","message":{"fragments":[{"type":"RESPONSE","content":"a"}]}}}}]"#;
        let bytes = build_zip(&[text_entry("conversations.json", conversations)]);
        let archive = parse_import_history(bytes).await.unwrap();
        assert_eq!(archive.format, "deepseek");
        assert_eq!(archive.sessions.len(), 1);
        assert_eq!(archive.sessions[0].platform_session_id, "conv-1");
        assert_eq!(archive.sessions[0].messages.len(), 2);
        assert_eq!(archive.sessions[0].messages[0].role, "user");
        assert_eq!(archive.sessions[0].messages[1].content, "a");
    }

    #[tokio::test]
    async fn sniffs_chatbox_zip_and_maps_content_parts() {
        let session = r#"{"id":"abc","name":"Chatbox 会话","messages":[
            {"id":"m1","role":"user","content":"旧版内容"},
            {"id":"m2","role":"assistant","model":"gpt","createdAt":"2026-08-01T10:00:00Z",
             "contentParts":[
                {"type":"text","text":"答"},
                {"type":"thinking","text":"推"},
                {"type":"tool_use","tool_calls":[{"name":"code_run","result":"done"}]}]}]}"#;
        let bytes = build_zip(&[
            text_entry(
                "manifest.json",
                r#"{"format":"chatbox-backup","version":1}"#,
            ),
            text_entry("sessions/abc/session.json", session),
        ]);
        let archive = parse_import_history(bytes).await.unwrap();
        assert_eq!(archive.format, "chatbox");
        let session = &archive.sessions[0];
        assert_eq!(session.platform, "chatbox");
        assert_eq!(session.platform_session_id, "abc");
        assert_eq!(session.messages[0].content, "旧版内容");
        let assistant = &session.messages[1];
        assert_eq!(assistant.content, "答");
        assert_eq!(assistant.metadata["thinking"], "推");
        assert_eq!(assistant.metadata["model"], "gpt");
        let tool_calls = assistant.metadata["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls[0]["name"], "code_run");
        assert_eq!(tool_calls[0]["result"], "done");
        assert!(assistant.created_at.is_some());
    }

    #[tokio::test]
    async fn rejects_chatbox_zip_with_foreign_manifest() {
        let bytes = build_zip(&[
            text_entry("manifest.json", r#"{"format":"someone-else"}"#),
            text_entry("sessions/abc/session.json", "{}"),
        ]);
        assert!(parse_import_history(bytes).await.is_err());
    }

    #[tokio::test]
    async fn parses_chatbox_legacy_flat_localstorage_json() {
        let dump = r#"{"session:abc":{"id":"abc","name":"旧会话","messages":[{"role":"user","content":"hi"}]},"other":"x"}"#;
        let archive = parse_import_history(dump.as_bytes().to_vec())
            .await
            .unwrap();
        assert_eq!(archive.format, "chatbox");
        assert_eq!(archive.sessions[0].platform_session_id, "abc");
        assert_eq!(archive.sessions[0].messages[0].content, "hi");
    }

    #[tokio::test]
    async fn parses_kelivo_chats_json_with_tool_events() {
        let chats = r#"{"version":1,
            "conversations":[{"id":"c1","title":"Kelivo 会话","createdAt":1756000000000}],
            "messages":[
                {"id":"m1","conversationId":"c1","role":"user","parts":[{"type":"text","text":"问"}],"createdAt":1756000001000},
                {"id":"m2","conversationId":"c1","role":"assistant",
                 "parts":[{"type":"reasoning","text":"思考"},{"type":"tool_call","toolCallId":"t1"},{"type":"text","text":"答"}],
                 "createdAt":1756000002000}],
            "toolEvents":[{"id":"t1","name":"web_search","arguments":{"q":"kelivo"},"result":"ok"}]}"#;
        let archive = parse_import_history(chats.as_bytes().to_vec())
            .await
            .unwrap();
        assert_eq!(archive.format, "kelivo");
        let session = &archive.sessions[0];
        assert_eq!(session.platform, "kelivo");
        assert_eq!(session.platform_session_id, "c1");
        assert_eq!(session.title, "Kelivo 会话");
        assert_eq!(session.messages[0].content, "问");
        let assistant = &session.messages[1];
        assert_eq!(assistant.metadata["thinking"], "思考");
        assert_eq!(assistant.content, "答");
        let tool_calls = assistant.metadata["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls[0]["name"], "web_search");
        assert_eq!(tool_calls[0]["args"]["q"], "kelivo");
        assert_eq!(tool_calls[0]["result"], "ok");
    }

    #[tokio::test]
    async fn parses_cherry_backup_sqlite_from_zip() {
        let workdir = std::env::temp_dir().join(format!("acm-cherry-test-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&workdir).await.unwrap();
        let db_path = workdir.join("cherrystudio.sqlite");
        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true);
        let pool = sqlx::SqlitePool::connect_with(options).await.unwrap();
        sqlx::query("CREATE TABLE topic (id TEXT PRIMARY KEY, name TEXT, created_at INTEGER)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE message (id TEXT PRIMARY KEY, topic_id TEXT, role TEXT, data TEXT, created_at INTEGER)")
            .execute(&pool)
            .await
            .unwrap();
        let data = serde_json::json!({
            "model": "gpt",
            "parts": [
                {"type": "text", "content": "你好"},
                {"type": "thinking", "content": "想一下"},
                {"type": "tool_calls", "tool_calls": [{"name": "web_search", "arguments": {"q": "x"}, "result": "found"}]}
            ]
        })
        .to_string();
        sqlx::query(
            "INSERT INTO topic (id, name, created_at) VALUES ('t1', 'Cherry 会话', 1756000000000)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO message (id, topic_id, role, data, created_at) VALUES ('m1', 't1', 'assistant', ?, 1756000001000)")
            .bind(&data)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        let sqlite_bytes = tokio::fs::read(&db_path).await.unwrap();
        let metadata = r#"{"appName":"Cherry Studio","version":"2.0"}"#;
        let bytes = build_zip(&[
            text_entry("metadata.json", metadata),
            ("cherrystudio.sqlite".to_owned(), sqlite_bytes),
        ]);
        let _ = tokio::fs::remove_dir_all(&workdir).await;

        let archive = parse_import_history(bytes).await.unwrap();
        assert_eq!(archive.format, "cherry");
        let session = &archive.sessions[0];
        assert_eq!(session.platform, "cherry");
        assert_eq!(session.platform_session_id, "t1");
        assert_eq!(session.title, "Cherry 会话");
        let message = &session.messages[0];
        assert_eq!(message.role, "assistant");
        assert_eq!(message.content, "你好");
        assert_eq!(message.metadata["thinking"], "想一下");
        assert_eq!(message.metadata["model"], "gpt");
        let tool_calls = message.metadata["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls[0]["name"], "web_search");
        assert_eq!(tool_calls[0]["args"]["q"], "x");
        assert_eq!(tool_calls[0]["result"], "found");
    }

    #[tokio::test]
    async fn parses_gemini_takeout_html_activities_deterministically() {
        let html = r#"<html><head><title>My Activity</title></head><body>
<div class="outer-cell mdl-shadow--2dp"><div class="mdl-grid"><div class="header-cell mdl-cell">Gemini Apps</div><div class="content-cell mdl-cell mdl-typography--body-1"><br>Prompted <b>你好</b><br>世界<br>2026年8月30日 12:34:56 GMT+8</div><div class="content-cell mdl-cell mdl-typography--body-1">回答 &lt;第一行&gt;<br>第二行 &amp;&amp; 更多</div></div></div>
<div class="outer-cell mdl-shadow--2dp"><div class="mdl-grid"><div class="content-cell mdl-cell"><br>Prompted 只有提示</div></div></div>
</body></html>"#;
        let first = parse_import_history(html.as_bytes().to_vec())
            .await
            .unwrap();
        let second = parse_import_history(html.as_bytes().to_vec())
            .await
            .unwrap();
        assert_eq!(first.format, "gemini");
        assert_eq!(first.sessions.len(), 2);
        let session = &first.sessions[0];
        assert_eq!(session.platform, "gemini");
        // 相同内容重复导入必须得到相同 platform_session_id 才能去重。
        assert_eq!(
            session.platform_session_id,
            second.sessions[0].platform_session_id
        );
        assert_eq!(session.messages[0].role, "user");
        assert_eq!(session.messages[0].content, "你好\n世界");
        assert_eq!(session.messages[1].role, "assistant");
        assert_eq!(session.messages[1].content, "回答 <第一行>\n第二行 && 更多");
        // 活动时间只进指纹，不进正文。
        assert!(!session.messages[0].content.contains("GMT"));
        let prompt_only = &first.sessions[1];
        assert_eq!(prompt_only.messages.len(), 1);
        assert_eq!(prompt_only.messages[0].content, "只有提示");
    }

    #[tokio::test]
    async fn rejects_unrecognized_inputs() {
        assert!(
            parse_import_history(b"not json at all {".to_vec())
                .await
                .is_err()
        );
        assert!(parse_import_history(b"{}".to_vec()).await.is_err());
        assert!(
            parse_import_history(r#"{"conversations":[]}"#.as_bytes().to_vec())
                .await
                .is_err()
        );
        let zip = build_zip(&[text_entry("foo.txt", "x")]);
        assert!(parse_import_history(zip).await.is_err());
        assert!(parse_import_history(Vec::new()).await.is_err());
    }
}
