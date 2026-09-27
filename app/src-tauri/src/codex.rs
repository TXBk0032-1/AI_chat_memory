//! OpenAI Codex rollout 解析：把磁盘 rollout JSONL 转成可入库的
//! NormalizedSession（用户消息 + 最终回答 + 过程说明），其余过程性内容
//! （思考、工具调用、diff、子代理、计划）产出 WorkItem 写入按会话的
//! 工作 JSONL。分页窗口按 thread_id 合并、按 ordinal 去重，子代理继承前缀
//! （ordinal < subagent_history_start_ordinal）跳过。
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::models::{NormalizedMessage, NormalizedSession, WorkItem};

/// 单条工作项正文的截断上限（UTF-8 字符边界安全）。
pub(crate) const WORK_ITEM_MAX_BYTES: usize = 130 * 1024;

/// 解析一个线程（可能由多个窗口文件合并）后的产物。
#[derive(Debug, Clone)]
pub struct CodexThread {
    pub session: NormalizedSession,
    pub work_items: Vec<WorkItem>,
    pub parent_platform_session_id: Option<String>,
}

/// rollout 单行。
#[derive(Debug, Clone)]
struct RolloutLine {
    ordinal: Option<i64>,
    kind: String,
    timestamp: Option<String>,
    payload: Value,
}

/// 按 UTF-8 字符边界把正文截断到 WORK_ITEM_MAX_BYTES，返回 (截断后文本, 是否截断)。
pub(crate) fn truncate_on_char_boundary(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

fn line_field<'a>(line: &'a Value, key: &str) -> Option<&'a Value> {
    line.get(key)
}

fn as_str(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

/// 取 cwd 的末段作为 project。
fn project_from_cwd(cwd: Option<&str>) -> Option<String> {
    let cwd = cwd?.trim_end_matches(['/', '\\']);
    let seg = cwd.rsplit(['/', '\\']).next()?;
    (!seg.is_empty()).then(|| seg.to_string())
}

pub fn parse_lines(
    thread_id: &str,
    raw_lines: &[Value],
    parent_hint: Option<&str>,
) -> crate::error::Result<CodexThread> {
    // 1. session_meta（首行或首个该类型行）
    let meta = raw_lines
        .iter()
        .find(|l| line_field(l, "type").and_then(Value::as_str) == Some("session_meta"))
        .and_then(|l| l.get("payload"))
        .cloned()
        .unwrap_or(Value::Null);
    let cwd = as_str(meta.get("cwd"));
    let project = project_from_cwd(cwd.as_deref());
    let created_at = crate::normalizer::normalize_timestamp(meta.get("timestamp"));
    let parent_platform_session_id = parent_hint
        .map(str::to_owned)
        .or_else(|| as_str(meta.get("parent_thread_id")));
    let agent_label = as_str(meta.get("agent_nickname"))
        .or_else(|| as_str(meta.get("agent_role")));
    let subagent_start = meta
        .get("subagent_history_start_ordinal")
        .and_then(Value::as_i64);

    // 2. 归一化并按 ordinal 去重（保留首个），跳过子代理继承前缀
    let mut seen_ordinals = std::collections::HashSet::new();
    let mut lines: Vec<RolloutLine> = Vec::new();
    for raw in raw_lines {
        let Some(kind) = as_str(line_field(raw, "type")) else { continue };
        if kind == "session_meta" {
            continue;
        }
        let ordinal = raw.get("ordinal").and_then(Value::as_i64);
        if let (Some(start), Some(ord)) = (subagent_start, ordinal)
            && ord < start
        {
            continue; // 继承自父会话的前缀，跳过
        }
        if let Some(ord) = ordinal
            && !seen_ordinals.insert(ord)
        {
            continue; // 分页窗口重叠，保留首个
        }
        lines.push(RolloutLine {
            ordinal,
            kind,
            timestamp: as_str(raw.get("timestamp")),
            payload: raw.get("payload").cloned().unwrap_or(Value::Null),
        });
    }

    // 3. 判定是否存在 item_completed
    let has_item_completed = lines.iter().any(|l| {
        l.kind == "event_msg"
            && l.payload.get("type").and_then(Value::as_str) == Some("item_completed")
    });

    let mut builder = ThreadBuilder::new();
    if has_item_completed {
        for l in &lines {
            if l.kind == "event_msg"
                && l.payload.get("type").and_then(Value::as_str) == Some("item_completed")
                && let Some(item) = l.payload.get("item")
            {
                builder.push_item(item, l.timestamp.as_deref());
            }
        }
    } else {
        builder.push_response_items(&lines);
    }

    let title = builder.derive_title();
    let updated_at = builder.last_timestamp.clone().or_else(|| created_at.clone());
    let session = NormalizedSession {
        id: uuid::Uuid::new_v4().to_string(),
        platform: "codex".into(),
        platform_session_id: thread_id.to_string(),
        title,
        created_at,
        updated_at,
        imported_at: chrono::Utc::now().to_rfc3339(),
        messages: builder.messages,
        raw_data: serde_json::json!({ "source": "codex", "thread_id": thread_id }),
        project,
        parent_platform_session_id: parent_platform_session_id.clone(),
        agent_label,
    };
    Ok(CodexThread {
        session,
        work_items: builder.work_items,
        parent_platform_session_id,
    })
}

#[derive(Default)]
struct ThreadBuilder {
    messages: Vec<NormalizedMessage>,
    work_items: Vec<WorkItem>,
    seq: i64,
    work_seq: i64,
    first_user_text: Option<String>,
    last_timestamp: Option<String>,
}

impl ThreadBuilder {
    fn new() -> Self {
        Self::default()
    }

    fn next_seq(&mut self) -> i64 {
        let s = self.seq;
        self.seq += 1;
        s
    }

    fn push_work(&mut self, seq: i64, kind: &str, title: String, body: Option<String>, expandable: bool) {
        let (body, truncated) = match body {
            Some(text) => {
                let (t, tr) = truncate_on_char_boundary(&text, WORK_ITEM_MAX_BYTES);
                (Some(t), tr)
            }
            None => (None, false),
        };
        self.work_items.push(WorkItem {
            work_seq: self.work_seq,
            seq,
            kind: kind.to_string(),
            title,
            body,
            expandable,
            truncated,
            agent_thread_id: None,
            agent_label: None,
        });
        self.work_seq += 1;
    }

    fn push_message(&mut self, role: &str, content: String, ts: Option<&str>) {
        self.messages.push(NormalizedMessage {
            role: role.to_string(),
            content,
            metadata: serde_json::json!({ "source": "codex" }),
            created_at: ts.map(str::to_owned),
        });
    }

    fn derive_title(&self) -> String {
        self.first_user_text
            .as_deref()
            .map(|t| t.chars().take(60).collect::<String>())
            .unwrap_or_default()
    }

    /// item_completed 路径：按 item.type 分流。
    fn push_item(&mut self, item: &Value, ts: Option<&str>) {
        if let Some(t) = ts {
            self.last_timestamp = Some(t.to_string());
        }
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        let seq = self.next_seq();
        match item_type {
            "UserMessage" => {
                let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
                if is_injected_user_text(text) {
                    return;
                }
                if self.first_user_text.is_none() && !text.is_empty() {
                    self.first_user_text = Some(text.to_string());
                }
                self.push_message("user", text.to_string(), ts);
            }
            "AgentMessage" => {
                let phase = item.get("phase").and_then(Value::as_str).unwrap_or("final_answer");
                let text = item.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
                match phase {
                    "commentary" => {
                        // 过程说明：既进 DB（可搜索）又进工作栏时间线
                        self.push_message("assistant", text.clone(), ts);
                        self.push_work(seq, "commentary", "过程说明".into(), Some(text), true);
                    }
                    _ => self.push_message("assistant", text, ts),
                }
            }
            "Reasoning" => {
                let summary = item
                    .get("summary_text")
                    .and_then(Value::as_array)
                    .map(|a| join_strings(a))
                    .unwrap_or_default();
                let raw = item
                    .get("raw_content")
                    .and_then(Value::as_array)
                    .map(|a| join_strings(a));
                let encrypted = raw.as_deref().map(str::is_empty).unwrap_or(true)
                    || item.get("encrypted_content").is_some();
                let title = if summary.is_empty() {
                    "思考".to_string()
                } else {
                    format!("思考 · {}", summary.lines().next().unwrap_or("").trim())
                };
                // 明文可展开；加密只留一行不可展开
                self.push_work(seq, "reasoning", title, raw.filter(|s| !s.is_empty()), !encrypted);
            }
            "CommandExecution" => {
                let cmd = item.get("command").and_then(Value::as_str).unwrap_or_default();
                let out = item.get("aggregated_output").and_then(Value::as_str).map(str::to_owned);
                self.push_work(seq, "command", format!("$ {cmd}"), out, true);
            }
            "McpToolCall" => {
                let name = item.get("tool").or_else(|| item.get("name")).and_then(Value::as_str).unwrap_or("tool");
                let body = item.get("result").map(compact_json).or_else(|| item.get("arguments").map(compact_json));
                self.push_work(seq, "mcp_tool", format!("MCP · {name}"), body, true);
            }
            "FunctionCallOutput" => {
                let body = item.get("output").and_then(Value::as_str).map(str::to_owned)
                    .or_else(|| Some(compact_json(item)));
                self.push_work(seq, "output", "工具输出".into(), body, true);
            }
            "FileChange" => {
                let body = render_file_change(item);
                self.push_work(seq, "file_change", "文件改动".into(), Some(body), true);
            }
            "SubAgentActivity" => {
                let kind = item.get("kind").and_then(Value::as_str).unwrap_or("update");
                let agent_thread_id = item.get("agent_thread_id").and_then(Value::as_str).map(str::to_owned);
                let label = item.get("agent_nickname").and_then(Value::as_str).unwrap_or("子代理");
                let verb = if kind.eq_ignore_ascii_case("start") { "已开始工作" } else { "已更新" };
                self.work_items.push(WorkItem {
                    work_seq: self.work_seq,
                    seq,
                    kind: "subagent".into(),
                    title: format!("子代理 {label} {verb}"),
                    body: None,
                    expandable: false,
                    truncated: false,
                    agent_thread_id,
                    agent_label: Some(label.to_string()),
                });
                self.work_seq += 1;
            }
            "Plan" => {
                let text = item.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
                self.push_work(seq, "plan", "计划".into(), Some(text), true);
            }
            // ContextCompaction 等忽略
            _ => {}
        }
    }
}

fn join_strings(arr: &[Value]) -> String {
    arr.iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n")
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn render_file_change(item: &Value) -> String {
    let Some(changes) = item.get("changes").and_then(Value::as_object) else {
        return compact_json(item);
    };
    let mut out = String::new();
    for (path, change) in changes {
        let kind = change.get("type").and_then(Value::as_str).unwrap_or("update");
        out.push_str(&format!("[{kind}] {path}\n"));
        if let Some(diff) = change.get("unified_diff").and_then(Value::as_str) {
            out.push_str(diff);
            out.push('\n');
        }
    }
    out
}

/// Codex 会把环境说明/工具注入以 UserMessage 形式写入，需从可见正文剔除。
fn is_injected_user_text(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<environment_context>")
        || t.starts_with("<user_instructions>")
        || t.starts_with("# Instructions")
        || t.starts_with("<INSTRUCTIONS>")
}

impl ThreadBuilder {
    /// 旧格式（无 item_completed）：走 response_item，按 call_id 配对工具调用/输出。
    fn push_response_items(&mut self, lines: &[RolloutLine]) {
        for l in lines {
            if l.kind != "response_item" {
                continue;
            }
            let p = &l.payload;
            let rtype = p.get("type").and_then(Value::as_str).unwrap_or_default();
            let seq = self.next_seq();
            match rtype {
                "message" => {
                    let role = p.get("role").and_then(Value::as_str).unwrap_or("assistant");
                    if role == "developer" {
                        continue; // 丢弃
                    }
                    let text = response_message_text(p);
                    if role == "user" {
                        if is_injected_user_text(&text) {
                            continue;
                        }
                        if self.first_user_text.is_none() && !text.is_empty() {
                            self.first_user_text = Some(text.clone());
                        }
                        self.push_message("user", text, l.timestamp.as_deref());
                    } else {
                        self.push_message("assistant", text, l.timestamp.as_deref());
                    }
                }
                "reasoning" => {
                    let summary = p.get("summary").and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(|s| s.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"))
                        .unwrap_or_default();
                    let body = p.get("content").and_then(Value::as_str).map(str::to_owned);
                    let encrypted = p.get("encrypted_content").is_some() && body.is_none();
                    let title = if summary.is_empty() { "思考".into() } else { format!("思考 · {}", summary.lines().next().unwrap_or("").trim()) };
                    self.push_work(seq, "reasoning", title, body, !encrypted);
                }
                "function_call" | "custom_tool_call" => {
                    let name = p.get("name").and_then(Value::as_str).unwrap_or("tool");
                    let args = p.get("arguments").or_else(|| p.get("input")).map(compact_json);
                    self.push_work(seq, "command", format!("$ {name}"), args, true);
                }
                "function_call_output" | "custom_tool_call_output" => {
                    let out = p.get("output").and_then(Value::as_str).map(str::to_owned).or_else(|| Some(compact_json(p)));
                    self.push_work(seq, "output", "工具输出".into(), out, true);
                }
                _ => {}
            }
        }
    }
}

fn response_message_text(p: &Value) -> String {
    // content 可能是字符串或 [{type, text}]
    match p.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|it| it.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

#[cfg(test)]
#[path = "codex/tests.rs"]
mod tests;
