use sqlx::{Row, SqlitePool};

use crate::{
    error::{AppError, Result},
    models::{
        BranchNode, BranchOverview, Message, Reference, SearchHitField, SessionOpen,
        SessionSearchHit,
    },
};

use super::sessions::summary_from_row;

const SESSION_BATCH_SIZE: i64 = 50;

pub async fn open_session(
    pool: &SqlitePool,
    id: &str,
    anchor_seq: Option<i64>,
) -> Result<SessionOpen> {
    let row = sqlx::query("SELECT id, platform, platform_session_id, title, created_at, updated_at, imported_at, raw_data FROM sessions WHERE id = ?")
        .bind(id).fetch_optional(pool).await?.ok_or_else(|| AppError::NotFound("session".into()))?;
    let references = row
        .try_get::<Option<String>, _>("raw_data")?
        .map(|raw| serde_json::from_str::<serde_json::Value>(&raw))
        .transpose()?
        .map_or_else(Vec::new, |raw| compact_references(&raw));
    let summary = summary_from_row(row);
    let message_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE session_id = ?")
            .bind(id)
            .fetch_one(pool)
            .await?;
    let start_seq = initial_start_seq(anchor_seq, message_count, SESSION_BATCH_SIZE);
    let messages = get_session_messages(pool, id, start_seq, SESSION_BATCH_SIZE).await?;
    let has_branches: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE session_id = ? AND json_extract(metadata, '$.source') = 'deepseek_export')",
    ).bind(id).fetch_one(pool).await?;
    Ok(SessionOpen {
        summary,
        message_count: message_count as usize,
        has_branches,
        start_seq,
        messages,
        references,
    })
}

pub async fn get_session_messages(
    pool: &SqlitePool,
    id: &str,
    start_seq: i64,
    limit: i64,
) -> Result<Vec<Message>> {
    let rows = sqlx::query("SELECT id, session_id, role, content, metadata, created_at, seq FROM messages WHERE session_id = ? AND seq >= ? ORDER BY seq LIMIT ?")
        .bind(id).bind(start_seq.max(0)).bind(limit.clamp(1, 100)).fetch_all(pool).await?;
    rows.into_iter().map(message_from_row).collect()
}

pub async fn search_session_hits(
    pool: &SqlitePool,
    id: &str,
    query: &str,
) -> Result<Vec<SessionSearchHit>> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query("SELECT id, seq, content, json_extract(metadata, '$.thinking') AS thinking FROM messages WHERE session_id = ? AND (instr(lower(content), ?) > 0 OR instr(lower(COALESCE(json_extract(metadata, '$.thinking'), '')), ?) > 0) ORDER BY seq")
        .bind(id).bind(&needle).bind(&needle).fetch_all(pool).await?;
    let mut hits = Vec::new();
    for row in rows {
        let message_id: String = row.try_get("id")?;
        let seq: i64 = row.try_get("seq")?;
        for (field, value) in [
            (
                SearchHitField::Content,
                row.try_get::<String, _>("content")?,
            ),
            (
                SearchHitField::Thinking,
                row.try_get::<Option<String>, _>("thinking")?
                    .unwrap_or_default(),
            ),
        ] {
            let count = occurrence_count(&value, &needle);
            if count > 0 {
                hits.push(SessionSearchHit {
                    message_id: message_id.clone(),
                    seq,
                    field,
                    count,
                });
            }
        }
    }
    Ok(hits)
}

pub async fn get_session_branches(pool: &SqlitePool, id: &str) -> Result<BranchOverview> {
    let raw_data: Option<String> = sqlx::query_scalar("SELECT raw_data FROM sessions WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("session".into()))?;
    let raw_data = raw_data
        .map(|raw| serde_json::from_str(&raw))
        .transpose()?
        .unwrap_or(serde_json::Value::Null);
    let rows = sqlx::query("SELECT id, role, content, metadata, seq FROM messages WHERE session_id = ? AND json_extract(metadata, '$.source') = 'deepseek_export' ORDER BY seq")
        .bind(id).fetch_all(pool).await?;
    let nodes = rows
        .into_iter()
        .map(|row| -> Result<BranchNode> {
            let metadata: serde_json::Value =
                serde_json::from_str(&row.try_get::<String, _>("metadata")?)?;
            let content: String = row.try_get("content")?;
            let thinking = metadata
                .get("thinking")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            Ok(BranchNode {
                message_id: row.try_get("id")?,
                seq: row.try_get("seq")?,
                role: row.try_get("role")?,
                node_id: json_string(&metadata, "node_id"),
                parent_node_id: json_string(&metadata, "parent_node_id"),
                children_node_ids: metadata
                    .get("children_node_ids")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                preview: truncate_preview(if content.is_empty() {
                    thinking
                } else {
                    &content
                }),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(crate::branch::build_overview(nodes, &raw_data))
}

fn initial_start_seq(anchor_seq: Option<i64>, message_count: i64, batch_size: i64) -> i64 {
    if message_count <= batch_size {
        return 0;
    }
    let anchor = anchor_seq.unwrap_or(0).clamp(0, message_count - 1);
    (anchor - batch_size / 2).clamp(0, message_count - batch_size)
}

fn message_from_row(row: sqlx::sqlite::SqliteRow) -> Result<Message> {
    Ok(Message {
        id: row.try_get("id")?,
        session_id: row.try_get("session_id")?,
        role: row.try_get("role")?,
        content: row.try_get("content")?,
        metadata: serde_json::from_str(&row.try_get::<String, _>("metadata")?)?,
        created_at: row.try_get("created_at")?,
        seq: row.try_get("seq")?,
    })
}

fn occurrence_count(value: &str, lowercase_needle: &str) -> usize {
    value.to_lowercase().match_indices(lowercase_needle).count()
}

fn json_string(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn truncate_preview(value: &str) -> String {
    const MAX_CHARS: usize = 160;
    let mut preview = value.replace(['\r', '\n'], " ");
    if let Some((index, _)) = preview.char_indices().nth(MAX_CHARS) {
        preview.truncate(index);
        preview.push('…');
    }
    preview
}

fn compact_references(raw: &serde_json::Value) -> Vec<Reference> {
    fn visit(
        value: &serde_json::Value,
        references: &mut std::collections::BTreeMap<i64, Reference>,
    ) {
        match value {
            serde_json::Value::Array(items) => {
                items.iter().for_each(|item| visit(item, references))
            }
            serde_json::Value::Object(object) => {
                let url = ["url", "link", "href"]
                    .iter()
                    .find_map(|key| object.get(*key).and_then(serde_json::Value::as_str));
                let cite_index = object
                    .get("cite_index")
                    .or_else(|| object.get("citeIndex"))
                    .or_else(|| object.get("index"))
                    .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()));
                if let (Some(url), Some(cite_index)) = (url, cite_index)
                    && cite_index >= 0
                    && (url.starts_with("http://") || url.starts_with("https://"))
                {
                    let string = |keys: &[&str]| {
                        keys.iter()
                            .find_map(|key| object.get(*key).and_then(serde_json::Value::as_str))
                    };
                    references.insert(
                        cite_index,
                        Reference {
                            cite_index,
                            url: url.to_owned(),
                            title: string(&["title", "name"]).unwrap_or_default().to_owned(),
                            summary: string(&["snippet", "summary", "description", "content"])
                                .unwrap_or_default()
                                .chars()
                                .take(280)
                                .collect(),
                        },
                    );
                }
                object.values().for_each(|item| visit(item, references));
            }
            _ => {}
        }
    }
    let mut references = std::collections::BTreeMap::new();
    visit(raw, &mut references);
    references.into_values().collect()
}
