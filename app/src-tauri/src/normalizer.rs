use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    error::{AppError, Result},
    models::{NormalizedMessage, NormalizedSession},
};

fn text(value: Option<&Value>) -> Option<String> {
    value.and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

fn normalize_timestamp(value: Option<&Value>) -> Option<String> {
    let raw = text(value)?;
    let trimmed = raw.trim();
    if let Ok(number) = trimmed.parse::<f64>() {
        if !number.is_finite() {
            return None;
        }
        let seconds = if number.abs() > 100_000_000_000.0 {
            number / 1000.0
        } else {
            number
        };
        return Some(seconds.to_string());
    }
    DateTime::parse_from_rfc3339(trimmed)
        .map(|dt| (dt.timestamp_millis() as f64 / 1000.0).to_string())
        .ok()
}

pub fn normalize_session(platform: &str, raw: &Value) -> Result<NormalizedSession> {
    let obj = raw
        .as_object()
        .ok_or_else(|| AppError::InvalidData("session must be an object".into()))?;
    let conversation = obj.get("_conversation");
    let (platform_id, title, created_at, updated_at, messages) = match platform {
        "deepseek" => (
            text(obj.get("id")),
            text(obj.get("title")).unwrap_or_default(),
            normalize_timestamp(obj.get("created_at")),
            normalize_timestamp(obj.get("updated_at")),
            normalize_deepseek_messages(conversation),
        ),
        "doubao" => {
            let conv = obj.get("conversation").and_then(Value::as_object);
            (
                conv.and_then(|v| text(v.get("conversation_id"))),
                conv.and_then(|v| text(v.get("name"))).unwrap_or_default(),
                conv.and_then(|v| normalize_timestamp(v.get("create_time"))),
                conv.and_then(|v| normalize_timestamp(v.get("update_time"))),
                normalize_doubao_messages(conversation),
            )
        }
        "kimi" => (
            text(obj.get("id")),
            text(obj.get("name")).unwrap_or_default(),
            normalize_timestamp(obj.get("createTime")),
            normalize_timestamp(obj.get("updateTime")),
            normalize_kimi_messages(conversation),
        ),
        _ => (
            text(obj.get("id")).or_else(|| Some(Uuid::new_v4().to_string())),
            text(obj.get("title")).unwrap_or_default(),
            normalize_timestamp(obj.get("created_at")),
            normalize_timestamp(obj.get("updated_at")),
            normalize_generic_messages(obj.get("messages")),
        ),
    };
    let platform_session_id = platform_id
        .filter(|id| !id.is_empty())
        .ok_or_else(|| AppError::InvalidData("platform session id is empty".into()))?;
    Ok(NormalizedSession {
        id: Uuid::new_v4().to_string(),
        platform: platform.into(),
        platform_session_id,
        title,
        created_at,
        updated_at,
        imported_at: Utc::now().to_rfc3339(),
        messages,
        raw_data: raw.clone(),
    })
}

/// DeepSeek fragment 中的工具调用（SEARCH/CODE 等非正文片段）统一提取为
/// `metadata.tool_calls` 条目：{name, result?, results_count?}。
fn deepseek_tool_calls_from_fragments(fragments: &[Value]) -> Vec<Value> {
    fragments
        .iter()
        .filter_map(|fragment| {
            let kind = fragment
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if matches!(kind, "REQUEST" | "RESPONSE" | "THINK" | "") {
                return None;
            }
            let mut call = serde_json::Map::new();
            call.insert("name".into(), Value::String(kind.to_string()));
            if let Some(content) = fragment.get("content").and_then(Value::as_str)
                && !content.is_empty()
            {
                call.insert("result".into(), Value::String(content.to_string()));
            }
            if let Some(results) = fragment.get("results").and_then(Value::as_array) {
                call.insert("results_count".into(), json!(results.len()));
            }
            Some(Value::Object(call))
        })
        .collect()
}

fn normalize_deepseek_messages(raw: Option<&Value>) -> Vec<NormalizedMessage> {
    let references = raw
        .and_then(|value| value.get("_references"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    raw.and_then(|v| v.pointer("/data/biz_data/chat_messages"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let fragments = m.get("fragments")?.as_array()?;
            let mut thinking = Vec::new();
            let mut content = Vec::new();
            for fragment in fragments {
                let kind = fragment
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                match kind {
                    "THINK" => {
                        if let Some(text) = fragment.get("content").and_then(Value::as_str) {
                            thinking.push(text);
                        }
                    }
                    "REQUEST" | "RESPONSE" | "" => {
                        if let Some(text) = fragment.get("content").and_then(Value::as_str) {
                            content.push(text);
                        }
                    }
                    _ => {}
                }
            }
            let thinking = thinking.join("\n");
            let tool_calls = deepseek_tool_calls_from_fragments(fragments);
            let mut metadata = serde_json::Map::new();
            metadata.insert(
                "model".into(),
                m.get("model").cloned().unwrap_or(Value::Null),
            );
            metadata.insert("references".into(), Value::Array(references.clone()));
            metadata.insert(
                "thinking".into(),
                if thinking.is_empty() {
                    Value::Null
                } else {
                    Value::String(thinking)
                },
            );
            if !tool_calls.is_empty() {
                metadata.insert("tool_calls".into(), Value::Array(tool_calls));
            }
            Some(NormalizedMessage {
                role: m
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("user")
                    .into(),
                content: content.join("\n"),
                metadata: Value::Object(metadata),
                created_at: normalize_timestamp(m.get("inserted_at"))
                    .or_else(|| normalize_timestamp(m.get("create_time"))),
            })
        })
        .collect()
}

fn normalize_doubao_messages(raw: Option<&Value>) -> Vec<NormalizedMessage> {
    raw.and_then(|v| v.pointer("/downlink_body/pull_singe_chain_downlink_body/messages"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|m| {
            let content = m
                .get("content")
                .and_then(|v| v.get("text"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| m.get("content").map(Value::to_string).unwrap_or_default());
            NormalizedMessage {
                role: if m.get("sender_type").and_then(Value::as_i64) == Some(2) {
                    "assistant"
                } else {
                    "user"
                }
                .into(),
                content,
                metadata: json!({}),
                created_at: normalize_timestamp(m.get("create_time")),
            }
        })
        .collect()
}

fn normalize_kimi_messages(raw: Option<&Value>) -> Vec<NormalizedMessage> {
    raw.and_then(|v| v.get("messages"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
            if role == "system" {
                return None;
            }
            let content = m
                .get("blocks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|b| b.pointer("/text/content").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            (!content.is_empty()).then(|| NormalizedMessage {
                role: role.into(),
                content,
                metadata: json!({}),
                created_at: normalize_timestamp(m.get("createTime")),
            })
        })
        .collect()
}

fn normalize_generic_messages(raw: Option<&Value>) -> Vec<NormalizedMessage> {
    raw.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            Some(NormalizedMessage {
                role: m.get("role")?.as_str()?.into(),
                content: m
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
                metadata: m.get("metadata").cloned().unwrap_or_else(|| json!({})),
                created_at: normalize_timestamp(m.get("created_at")),
            })
        })
        .collect()
}

pub fn normalize_deepseek_export(raw: &Value) -> Result<NormalizedSession> {
    let mapping = raw
        .get("mapping")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            AppError::InvalidData("DeepSeek export conversation missing mapping".into())
        })?;
    let mut messages = Vec::new();
    for (node_id, node) in mapping {
        let Some(message) = node.get("message") else {
            continue;
        };
        let fragments = message
            .get("fragments")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut user = Vec::new();
        let mut assistant = Vec::new();
        let mut thinking = Vec::new();
        let mut types = Vec::new();
        let mut tool_types = Vec::new();
        let mut search_result_count = 0usize;
        let mut references = Vec::new();
        for fragment in &fragments {
            let kind = fragment
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !kind.is_empty() {
                types.push(kind.to_string());
            }
            if let Some(content) = fragment.get("content").and_then(Value::as_str) {
                match kind {
                    "REQUEST" => user.push(content),
                    "RESPONSE" => assistant.push(content),
                    "THINK" => thinking.push(content),
                    _ => {}
                }
            }
            if !matches!(kind, "REQUEST" | "RESPONSE" | "THINK" | "") {
                tool_types.push(kind.to_string());
            }
            search_result_count += fragment
                .get("results")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            if let Some(results) = fragment.get("results").and_then(Value::as_array) {
                references.extend(results.iter().cloned());
            }
        }
        let tool_calls = deepseek_tool_calls_from_fragments(&fragments);
        let (role, content) = if !user.is_empty() {
            ("user", user.join("\n"))
        } else if !assistant.is_empty() {
            ("assistant", assistant.join("\n"))
        } else {
            continue;
        };
        let mut metadata = serde_json::Map::new();
        metadata.insert("source".into(), json!("deepseek_export"));
        metadata.insert(
            "node_id".into(),
            json!(node.get("id").and_then(Value::as_str).unwrap_or(node_id)),
        );
        metadata.insert("parent_node_id".into(), json!(node.get("parent")));
        metadata.insert(
            "children_node_ids".into(),
            node.get("children").cloned().unwrap_or_else(|| json!([])),
        );
        metadata.insert("fragment_types".into(), json!(types));
        metadata.insert("tool_types".into(), json!(tool_types));
        metadata.insert("search_result_count".into(), json!(search_result_count));
        metadata.insert("references".into(), Value::Array(references));
        metadata.insert(
            "model".into(),
            message.get("model").cloned().unwrap_or(Value::Null),
        );
        metadata.insert(
            "files".into(),
            message.get("files").cloned().unwrap_or_else(|| json!([])),
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
        messages.push(NormalizedMessage {
            role: role.into(),
            content,
            metadata: Value::Object(metadata),
            created_at: normalize_timestamp(message.get("inserted_at"))
                .or_else(|| normalize_timestamp(raw.get("updated_at")))
                .or_else(|| normalize_timestamp(raw.get("inserted_at"))),
        });
    }
    messages.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    let platform_session_id = text(raw.get("id"))
        .filter(|id| !id.is_empty())
        .ok_or_else(|| AppError::InvalidData("export session id is empty".into()))?;
    Ok(NormalizedSession {
        id: Uuid::new_v4().to_string(),
        platform: "deepseek".into(),
        platform_session_id,
        title: text(raw.get("title")).unwrap_or_default(),
        created_at: normalize_timestamp(raw.get("inserted_at")),
        updated_at: normalize_timestamp(raw.get("updated_at"))
            .or_else(|| normalize_timestamp(raw.get("inserted_at"))),
        imported_at: Utc::now().to_rfc3339(),
        messages,
        raw_data: raw.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_supported_platforms() {
        let deepseek = normalize_session("deepseek", &json!({"id":"d","title":"D","_conversation":{"data":{"biz_data":{"chat_messages":[{"role":"assistant","fragments":[{"type":"THINK","content":"t"},{"type":"RESPONSE","content":"a"}]}]}}}})).unwrap();
        assert_eq!(deepseek.messages[0].content, "a");
        assert_eq!(deepseek.messages[0].metadata["thinking"], "t");
        let doubao = normalize_session("doubao", &json!({"conversation":{"conversation_id":"b","name":"B"},"_conversation":{"downlink_body":{"pull_singe_chain_downlink_body":{"messages":[{"sender_type":2,"content":{"text":"a"}}]}}}})).unwrap();
        assert_eq!(doubao.messages[0].role, "assistant");
        let kimi = normalize_session("kimi", &json!({"id":"k","name":"K","_conversation":{"messages":[{"role":"user","blocks":[{"text":{"content":"q"}}]}]}})).unwrap();
        assert_eq!(kimi.messages[0].content, "q");
    }

    #[test]
    fn extracts_tool_calls_from_deepseek_live_messages() {
        let session = normalize_session(
            "deepseek",
            &json!({
                "id": "d",
                "title": "D",
                "_conversation": {
                    "data": {"biz_data": {"chat_messages": [
                        {"role": "assistant", "fragments": [
                            {"type": "THINK", "content": "t"},
                            {"type": "SEARCH", "results": [{"title": "S", "url": "https://example.com"}]},
                            {"type": "RESPONSE", "content": "a"}
                        ]}
                    ]}}
                }
            }),
        )
        .unwrap();

        let message = &session.messages[0];
        assert_eq!(message.content, "a");
        let tool_calls = message.metadata["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["name"], "SEARCH");
        assert_eq!(tool_calls[0]["results_count"], 1);
        assert!(message.metadata.get("tool_calls").is_some());
    }

    #[test]
    fn extracts_tool_calls_from_deepseek_export_messages() {
        let session = normalize_deepseek_export(&json!({
            "id": "conversation",
            "mapping": {
                "node": {
                    "id": "node",
                    "message": {
                        "fragments": [
                            {"type": "THINK", "content": "t"},
                            {"type": "CODE_INTERPRETER", "content": "ran cells"},
                            {"type": "RESPONSE", "content": "answer"}
                        ]
                    }
                }
            }
        }))
        .unwrap();

        let message = &session.messages[0];
        assert_eq!(message.content, "answer");
        assert_eq!(message.metadata["thinking"], "t");
        let tool_calls = message.metadata["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["name"], "CODE_INTERPRETER");
        assert_eq!(tool_calls[0]["result"], "ran cells");
        // 旧计数字段保留，便于既有消费者与导出对照。
        assert_eq!(message.metadata["tool_types"], json!(["CODE_INTERPRETER"]));
    }

    #[test]
    fn preserves_references_captured_by_deepseek_userscript() {
        let session = normalize_session(
            "deepseek",
            &json!({
                "id": "d",
                "title": "D",
                "_conversation": {
                    "_references": [{"cite_index": 35, "url": "https://example.com", "title": "Example", "snippet": "Summary"}],
                    "data": {"biz_data": {"chat_messages": [{"role": "assistant", "fragments": [{"type": "RESPONSE", "content": "answer [reference:35]"}]}]}}
                }
            }),
        )
        .unwrap();

        assert_eq!(
            session.messages[0].metadata["references"][0]["cite_index"],
            35
        );
        assert_eq!(
            session.messages[0].metadata["references"][0]["url"],
            "https://example.com"
        );
    }

    #[test]
    fn normalizes_iso_seconds_and_milliseconds_to_epoch_seconds() {
        assert_eq!(
            normalize_timestamp(Some(&json!("2026-06-08T01:35:06.105+08:00"))),
            Some("1780853706.105".into())
        );
        assert_eq!(
            normalize_timestamp(Some(&json!(1780853706105_i64))),
            Some("1780853706.105".into())
        );
        assert_eq!(
            normalize_timestamp(Some(&json!(1780853706.105))),
            Some("1780853706.105".into())
        );
    }

    #[test]
    fn preserves_deepseek_export_references_for_rendering() {
        let session = normalize_deepseek_export(&json!({
            "id": "conversation",
            "mapping": {
                "node": {
                    "id": "node",
                    "message": {
                        "fragments": [
                            {"type": "RESPONSE", "content": "answer [reference:0]"},
                            {"type": "SEARCH", "results": [{"title": "Source", "url": "https://example.com", "snippet": "Summary"}]}
                        ]
                    }
                }
            }
        }))
        .unwrap();

        assert_eq!(session.messages[0].metadata["search_result_count"], 1);
        assert_eq!(
            session.messages[0].metadata["references"][0]["url"],
            "https://example.com"
        );
        assert_eq!(
            session.messages[0].metadata["references"][0]["snippet"],
            "Summary"
        );
    }
}
