mod connection;
mod timestamp;

pub use connection::{connect, copy_database};

#[cfg(test)]
use connection::normalize_stored_timestamps;
#[cfg(test)]
use sqlx::sqlite::SqlitePoolOptions;

use sqlx::{Row, SqlitePool};

use crate::{
    error::{AppError, Result},
    models::{
        BranchNode, BranchOverview, Message, NormalizedSession, Reference, SearchHitField,
        SearchQuery, SessionOpen, SessionSearchHit, SessionSummary,
    },
};

fn timestamp_expression(column: &str) -> String {
    timestamp::expression(column)
}

pub async fn import_sessions(pool: &SqlitePool, sessions: &[NormalizedSession]) -> Result<usize> {
    let mut tx = pool.begin().await?;
    for session in sessions {
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT id FROM sessions WHERE platform = ? AND platform_session_id = ?",
        )
        .bind(&session.platform)
        .bind(&session.platform_session_id)
        .fetch_optional(&mut *tx)
        .await?;
        let id = existing.unwrap_or_else(|| session.id.clone());
        sqlx::query("INSERT INTO sessions (id, platform, platform_session_id, title, created_at, updated_at, imported_at, raw_data) VALUES (?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(platform, platform_session_id) DO UPDATE SET title=excluded.title, created_at=excluded.created_at, updated_at=excluded.updated_at, imported_at=excluded.imported_at, raw_data=excluded.raw_data")
            .bind(&id).bind(&session.platform).bind(&session.platform_session_id).bind(&session.title)
            .bind(&session.created_at).bind(&session.updated_at).bind(&session.imported_at).bind(serde_json::to_string(&session.raw_data)?).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM messages WHERE session_id = ?")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        for (seq, message) in session.messages.iter().enumerate() {
            sqlx::query("INSERT INTO messages (id, session_id, role, content, metadata, created_at, seq) VALUES (?, ?, ?, ?, ?, ?, ?)")
                .bind(format!("{id}_{seq}")).bind(&id).bind(&message.role).bind(&message.content)
                .bind(serde_json::to_string(&message.metadata)?).bind(&message.created_at).bind(seq as i64).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    Ok(sessions.len())
}

pub async fn search(pool: &SqlitePool, query: &SearchQuery) -> Result<Vec<SessionSummary>> {
    let timestamp = timestamp_expression("s.updated_at");
    let sql = format!(
        "SELECT s.id, s.platform, s.platform_session_id, s.title, s.created_at, s.updated_at, s.imported_at FROM sessions s WHERE (? IS NULL OR s.platform = ?) AND (? IS NULL OR ({timestamp}) >= CAST(? AS REAL)) AND (? IS NULL OR ({timestamp}) <= CAST(? AS REAL)) AND (? IS NULL OR s.title LIKE '%' || ? || '%' OR EXISTS (SELECT 1 FROM messages m WHERE m.session_id=s.id AND m.content LIKE '%' || ? || '%')) ORDER BY ({timestamp}) DESC LIMIT ? OFFSET ?"
    );
    let rows = sqlx::query(&sql)
        .bind(&query.platform)
        .bind(&query.platform)
        .bind(&query.date_from)
        .bind(&query.date_from)
        .bind(&query.date_to)
        .bind(&query.date_to)
        .bind(&query.q)
        .bind(&query.q)
        .bind(&query.q)
        .bind(query.limit.unwrap_or(500).clamp(1, 1000))
        .bind(query.offset.unwrap_or(0).max(0))
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(summary_from_row).collect())
}

pub async fn count(pool: &SqlitePool, query: &SearchQuery) -> Result<i64> {
    let timestamp = timestamp_expression("s.updated_at");
    let sql = format!(
        "SELECT COUNT(*) FROM sessions s WHERE (? IS NULL OR s.platform = ?) AND (? IS NULL OR ({timestamp}) >= CAST(? AS REAL)) AND (? IS NULL OR ({timestamp}) <= CAST(? AS REAL)) AND (? IS NULL OR s.title LIKE '%' || ? || '%' OR EXISTS (SELECT 1 FROM messages m WHERE m.session_id=s.id AND m.content LIKE '%' || ? || '%'))"
    );
    Ok(sqlx::query_scalar(&sql)
        .bind(&query.platform)
        .bind(&query.platform)
        .bind(&query.date_from)
        .bind(&query.date_from)
        .bind(&query.date_to)
        .bind(&query.date_to)
        .bind(&query.q)
        .bind(&query.q)
        .bind(&query.q)
        .fetch_one(pool)
        .await?)
}

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
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
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
        .bind(id)
        .bind(start_seq.max(0))
        .bind(limit.clamp(1, 100))
        .fetch_all(pool)
        .await?;
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
        .bind(id)
        .bind(&needle)
        .bind(&needle)
        .fetch_all(pool)
        .await?;
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
        .bind(id)
        .fetch_all(pool)
        .await?;
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
                for item in items {
                    visit(item, references);
                }
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
                for item in object.values() {
                    visit(item, references);
                }
            }
            _ => {}
        }
    }

    let mut references = std::collections::BTreeMap::new();
    visit(raw, &mut references);
    references.into_values().collect()
}

pub async fn delete_session(pool: &SqlitePool, id: &str) -> Result<()> {
    let result = sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("session".into()));
    }
    Ok(())
}

pub async fn sync_status(pool: &SqlitePool, platform: &str) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT CAST(MAX(CAST(updated_at AS REAL)) AS TEXT) FROM sessions WHERE platform = ?",
    )
    .bind(platform)
    .fetch_one(pool)
    .await?)
}

fn summary_from_row(row: sqlx::sqlite::SqliteRow) -> SessionSummary {
    SessionSummary {
        id: row.get("id"),
        platform: row.get("platform"),
        platform_session_id: row.get("platform_session_id"),
        title: row.get::<Option<String>, _>("title").unwrap_or_default(),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        imported_at: row.get("imported_at"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalizer::normalize_session;
    use serde_json::json;

    async fn create_detail_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE sessions (id TEXT PRIMARY KEY, platform TEXT NOT NULL, platform_session_id TEXT NOT NULL, title TEXT, created_at TEXT, updated_at TEXT, imported_at TEXT, raw_data TEXT, UNIQUE(platform, platform_session_id)); CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, role TEXT NOT NULL, content TEXT, metadata TEXT, created_at TEXT, seq INTEGER);")
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    #[tokio::test]
    async fn opens_bounded_window_around_anchor_without_raw_data() {
        let pool = create_detail_pool().await;
        let raw = json!({
            "payload": "x".repeat(1024 * 1024),
            "sources": [{"cite_index": 35, "url": "https://example.com", "title": "Example", "snippet": "Summary"}]
        });
        sqlx::query("INSERT INTO sessions (id, platform, platform_session_id, title, raw_data) VALUES ('long', 'deepseek', 'long', 'Long', ?)")
            .bind(raw.to_string())
            .execute(&pool)
            .await
            .unwrap();
        for seq in 0..1000_i64 {
            sqlx::query("INSERT INTO messages (id, session_id, role, content, metadata, seq) VALUES (?, 'long', 'assistant', ?, '{}', ?)")
                .bind(format!("message-{seq}"))
                .bind(format!("content {seq}"))
                .bind(seq)
                .execute(&pool)
                .await
                .unwrap();
        }

        let opened = open_session(&pool, "long", Some(500)).await.unwrap();
        assert_eq!(opened.message_count, 1000);
        assert_eq!(opened.start_seq, 475);
        assert_eq!(opened.messages.len(), 50);
        assert_eq!(opened.messages[0].seq, 475);
        assert_eq!(opened.messages[49].seq, 524);
        assert_eq!(opened.references.len(), 1);
        assert_eq!(opened.references[0].cite_index, 35);
        assert_eq!(opened.references[0].summary, "Summary");

        let bounded = get_session_messages(&pool, "long", 0, 1000).await.unwrap();
        assert_eq!(bounded.len(), 100);
    }

    #[tokio::test]
    async fn returns_full_text_hits_and_lightweight_branches() {
        let pool = create_detail_pool().await;
        sqlx::query("INSERT INTO sessions (id, platform, platform_session_id, title, raw_data) VALUES ('branch', 'deepseek', 'branch', 'Branch', '{}')")
            .execute(&pool)
            .await
            .unwrap();
        let metadata = json!({
            "source": "deepseek_export",
            "node_id": "node-1",
            "parent_node_id": "root",
            "children_node_ids": ["node-2"],
            "thinking": "Needle in thinking, needle again"
        });
        sqlx::query("INSERT INTO messages (id, session_id, role, content, metadata, seq) VALUES ('message-1', 'branch', 'assistant', ?, ?, 12)")
            .bind(format!("Needle in content {}", "x".repeat(300)))
            .bind(metadata.to_string())
            .execute(&pool)
            .await
            .unwrap();

        let hits = search_session_hits(&pool, "branch", "needle")
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].field, SearchHitField::Content);
        assert_eq!(hits[0].count, 1);
        assert_eq!(hits[1].field, SearchHitField::Thinking);
        assert_eq!(hits[1].count, 2);

        let branches = get_session_branches(&pool, "branch").await.unwrap();
        assert_eq!(branches.nodes.len(), 1);
        assert_eq!(branches.nodes[0].node_id, "node-1");
        assert!(branches.nodes[0].children_node_ids.is_empty());
        assert_eq!(branches.nodes[0].preview.chars().count(), 161);
        assert!(branches.nodes[0].preview.ends_with('…'));
        assert_eq!(branches.default_leaf_node_id, "node-1");
    }

    #[tokio::test]
    async fn reimport_preserves_identity_and_replaces_messages() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("PRAGMA foreign_keys=ON")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE sessions (id TEXT PRIMARY KEY, platform TEXT NOT NULL, platform_session_id TEXT NOT NULL, title TEXT, created_at TEXT, updated_at TEXT, imported_at TEXT, raw_data TEXT, UNIQUE(platform, platform_session_id)); CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, role TEXT NOT NULL, content TEXT, metadata TEXT, created_at TEXT, seq INTEGER);").execute(&pool).await.unwrap();
        let first = normalize_session(
            "custom",
            &json!({"id":"a","title":"one","messages":[{"role":"user","content":"old"}]}),
        )
        .unwrap();
        import_sessions(&pool, &[first]).await.unwrap();
        let original: String = sqlx::query_scalar("SELECT id FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        let second = normalize_session(
            "custom",
            &json!({"id":"a","title":"two","messages":[{"role":"assistant","content":"new"}]}),
        )
        .unwrap();
        import_sessions(&pool, &[second]).await.unwrap();
        let current: String = sqlx::query_scalar("SELECT id FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        let content: String = sqlx::query_scalar("SELECT content FROM messages")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(original, current);
        assert_eq!(content, "new");
    }

    #[tokio::test]
    async fn search_is_bound_and_delete_cascades() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("PRAGMA foreign_keys=ON")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE sessions (id TEXT PRIMARY KEY, platform TEXT NOT NULL, platform_session_id TEXT NOT NULL, title TEXT, created_at TEXT, updated_at TEXT, imported_at TEXT, raw_data TEXT, UNIQUE(platform, platform_session_id)); CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, role TEXT NOT NULL, content TEXT, metadata TEXT, created_at TEXT, seq INTEGER);").execute(&pool).await.unwrap();
        let session = normalize_session(
            "custom",
            &json!({"id":"safe","title":"safe","messages":[{"role":"user","content":"hello"}]}),
        )
        .unwrap();
        import_sessions(&pool, &[session]).await.unwrap();
        let result = search(
            &pool,
            &SearchQuery {
                q: Some("%' OR 1=1 --".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(result.is_empty());
        let id: String = sqlx::query_scalar("SELECT id FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        delete_session(&pool, &id).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn normalizes_mixed_timestamps_and_filters_by_epoch_range() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE sessions (id TEXT PRIMARY KEY, platform TEXT NOT NULL, platform_session_id TEXT NOT NULL, title TEXT, created_at TEXT, updated_at TEXT, imported_at TEXT, raw_data TEXT, UNIQUE(platform, platform_session_id)); CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, role TEXT NOT NULL, content TEXT, metadata TEXT, created_at TEXT, seq INTEGER);")
            .execute(&pool)
            .await
            .unwrap();
        for (id, updated_at) in [
            ("iso", "2026-06-08T01:35:06.105+08:00"),
            ("seconds", "1780853706.105"),
            ("milliseconds", "1780853706105"),
            ("older", "1749317706.105"),
        ] {
            sqlx::query("INSERT INTO sessions (id, platform, platform_session_id, title, updated_at) VALUES (?, 'deepseek', ?, ?, ?)")
                .bind(id)
                .bind(id)
                .bind(id)
                .bind(updated_at)
                .execute(&pool)
                .await
                .unwrap();
        }
        normalize_stored_timestamps(&pool).await.unwrap();
        let rows = search(
            &pool,
            &SearchQuery {
                date_from: Some("1780853706".into()),
                date_to: Some("1780853707".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.id != "older"));
        let distinct: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT updated_at) FROM sessions WHERE id != 'older'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(distinct, 1);
    }
}
