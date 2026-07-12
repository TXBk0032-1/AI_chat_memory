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

fn normalize_deepseek_messages(raw: Option<&Value>) -> Vec<NormalizedMessage> {
    let references = raw
        .and_then(|value| value.get("_references"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    raw.and_then(|v| v.pointer("/data/biz_data/chat_messages")).and_then(Value::as_array)
        .into_iter().flatten().filter_map(|m| {
            let fragments = m.get("fragments")?.as_array()?;
            let thinking = fragments.iter().filter(|f| f.get("type").and_then(Value::as_str) == Some("THINK"))
                .filter_map(|f| f.get("content").and_then(Value::as_str)).collect::<Vec<_>>().join("\n");
            let content = fragments.iter().filter(|f| f.get("type").and_then(Value::as_str) != Some("THINK"))
                .filter_map(|f| f.get("content").and_then(Value::as_str)).collect::<Vec<_>>().join("\n");
            Some(NormalizedMessage { role: m.get("role").and_then(Value::as_str).unwrap_or("user").into(), content,
                metadata: json!({"model": m.get("model"), "references": references, "thinking": if thinking.is_empty() { Value::Null } else { Value::String(thinking) }}),
                created_at: normalize_timestamp(m.get("inserted_at")).or_else(|| normalize_timestamp(m.get("create_time"))) })
        }).collect()
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
        let (role, content) = if !user.is_empty() {
            ("user", user.join("\n"))
        } else if !assistant.is_empty() {
            ("assistant", assistant.join("\n"))
        } else {
            continue;
        };
        messages.push(NormalizedMessage { role: role.into(), content,
            metadata: json!({"source":"deepseek_export","node_id":node.get("id").and_then(Value::as_str).unwrap_or(node_id),
                "parent_node_id":node.get("parent"),"children_node_ids":node.get("children").cloned().unwrap_or_else(|| json!([])),
                "fragment_types":types,"tool_types":tool_types,"search_result_count":search_result_count,"references":references,
                "model":message.get("model"),"files":message.get("files").cloned().unwrap_or_else(||json!([])),
                "thinking":if thinking.is_empty(){Value::Null}else{Value::String(thinking.join("\n"))}}),
            created_at: normalize_timestamp(message.get("inserted_at")).or_else(|| normalize_timestamp(raw.get("updated_at"))).or_else(|| normalize_timestamp(raw.get("inserted_at"))) });
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
