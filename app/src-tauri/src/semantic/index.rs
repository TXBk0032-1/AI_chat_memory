use sqlx::{Row, SqlitePool};
use std::collections::{HashMap, HashSet};

use super::chunker::chunk_message;
use crate::{
    database,
    embedding::BackendIdentity,
    error::Result,
    models::{SearchHitField, SearchQuery, SessionSearchHit, SessionSummary},
};

use crate::database::timestamp;

#[derive(Debug, Clone)]
pub struct PendingChunk {
    pub id: i64,
    pub text: String,
}

pub async fn queue_session_chunks(
    pool: &SqlitePool,
    session_id: &str,
    identity: &BackendIdentity,
) -> Result<usize> {
    let session = sqlx::query("SELECT id, platform, title FROM sessions WHERE id = ?")
        .bind(session_id)
        .fetch_optional(pool)
        .await?;
    let Some(session) = session else {
        return Ok(0);
    };
    let platform: String = session.try_get("platform")?;
    let title: String = session.try_get("title")?;
    let messages =
        sqlx::query("SELECT id, role, content FROM messages WHERE session_id = ? ORDER BY seq ASC")
            .bind(session_id)
            .fetch_all(pool)
            .await?;

    let mut desired = Vec::new();
    for message in messages {
        let message_id: String = message.try_get("id")?;
        let role: String = message.try_get("role")?;
        let content: String = message.try_get("content")?;
        desired.extend(chunk_message(
            &platform,
            &title,
            &message_id,
            session_id,
            &role,
            &content,
        ));
    }

    let existing = sqlx::query(
        "SELECT id, message_id, chunk_index, content_hash, status FROM embedding_chunks WHERE session_id = ? AND backend_id = ? AND model_id = ?",
    )
    .bind(session_id)
    .bind(&identity.backend_id)
    .bind(&identity.model_id)
    .fetch_all(pool)
    .await?;

    let mut existing_map = HashMap::new();
    for row in existing {
        let message_id: String = row.try_get("message_id")?;
        let chunk_index: i64 = row.try_get("chunk_index")?;
        existing_map.insert((message_id, chunk_index), row);
    }

    let mut keep = HashSet::new();
    let now = chrono::Utc::now().to_rfc3339();
    let mut queued = 0usize;

    for chunk in desired {
        keep.insert((chunk.message_id.clone(), chunk.chunk_index));
        if let Some(row) = existing_map.get(&(chunk.message_id.clone(), chunk.chunk_index)) {
            let content_hash: String = row.try_get("content_hash")?;
            let status: String = row.try_get("status")?;
            let id: i64 = row.try_get("id")?;
            if content_hash == chunk.content_hash && status == "ready" {
                continue;
            }
            sqlx::query(
                "UPDATE embedding_chunks SET text = ?, content_hash = ?, role = ?, dim = ?, status = 'pending', error = NULL, updated_at = ? WHERE id = ?",
            )
            .bind(&chunk.text)
            .bind(&chunk.content_hash)
            .bind(&chunk.role)
            .bind(identity.dimensions as i64)
            .bind(&now)
            .bind(id)
            .execute(pool)
            .await?;
            let _ = sqlx::query("DELETE FROM embedding_vec WHERE chunk_id = ?")
                .bind(id)
                .execute(pool)
                .await;
            queued += 1;
        } else {
            sqlx::query(
                "INSERT INTO embedding_chunks (message_id, session_id, platform, chunk_index, role, text, content_hash, backend_id, model_id, dim, status, error, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', NULL, ?)",
            )
            .bind(&chunk.message_id)
            .bind(&chunk.session_id)
            .bind(&chunk.platform)
            .bind(chunk.chunk_index)
            .bind(&chunk.role)
            .bind(&chunk.text)
            .bind(&chunk.content_hash)
            .bind(&identity.backend_id)
            .bind(&identity.model_id)
            .bind(identity.dimensions as i64)
            .bind(&now)
            .execute(pool)
            .await?;
            queued += 1;
        }
    }

    for ((message_id, chunk_index), row) in existing_map {
        if keep.contains(&(message_id, chunk_index)) {
            continue;
        }
        let id: i64 = row.try_get("id")?;
        let _ = sqlx::query("DELETE FROM embedding_vec WHERE chunk_id = ?")
            .bind(id)
            .execute(pool)
            .await;
        sqlx::query("DELETE FROM embedding_chunks WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await?;
    }

    Ok(queued)
}

pub async fn queue_all_sessions(pool: &SqlitePool, identity: &BackendIdentity) -> Result<usize> {
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM sessions")
        .fetch_all(pool)
        .await?;
    let mut total = 0usize;
    for id in ids {
        total += queue_session_chunks(pool, &id, identity).await?;
    }
    Ok(total)
}

pub async fn delete_session_chunks(pool: &SqlitePool, session_id: &str) -> Result<()> {
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM embedding_chunks WHERE session_id = ?")
        .bind(session_id)
        .fetch_all(pool)
        .await?;
    for id in ids {
        let _ = sqlx::query("DELETE FROM embedding_vec WHERE chunk_id = ?")
            .bind(id)
            .execute(pool)
            .await;
    }
    sqlx::query("DELETE FROM embedding_chunks WHERE session_id = ?")
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn fetch_pending_chunks(
    pool: &SqlitePool,
    identity: &BackendIdentity,
    limit: i64,
) -> Result<Vec<PendingChunk>> {
    let rows = sqlx::query(
        "SELECT id, text FROM embedding_chunks WHERE status = 'pending' AND backend_id = ? AND model_id = ? ORDER BY id ASC LIMIT ?",
    )
    .bind(&identity.backend_id)
    .bind(&identity.model_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| PendingChunk {
            id: row.get("id"),
            text: row.get("text"),
        })
        .collect())
}

pub async fn mark_chunk_ready(
    pool: &SqlitePool,
    chunk_id: i64,
    identity: &BackendIdentity,
    session_id: &str,
    message_id: &str,
    platform: &str,
    embedding: &[f32],
) -> Result<()> {
    let mut vector = embedding.to_vec();
    if vector.len() < 640 {
        vector.resize(640, 0.0);
    } else if vector.len() > 640 {
        vector.truncate(640);
    }
    let bytes = f32_slice_as_bytes(&vector);
    let _ = sqlx::query("DELETE FROM embedding_vec WHERE chunk_id = ?")
        .bind(chunk_id)
        .execute(pool)
        .await;
    sqlx::query(
        "INSERT INTO embedding_vec(chunk_id, embedding, session_id, message_id, platform) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(chunk_id)
    .bind(bytes)
    .bind(session_id)
    .bind(message_id)
    .bind(platform)
    .execute(pool)
    .await?;
    sqlx::query(
        "UPDATE embedding_chunks SET status = 'ready', error = NULL, dim = ?, updated_at = ? WHERE id = ?",
    )
    .bind(identity.dimensions as i64)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(chunk_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn mark_chunk_error(pool: &SqlitePool, chunk_id: i64, error: &str) -> Result<()> {
    sqlx::query(
        "UPDATE embedding_chunks SET status = 'error', error = ?, updated_at = ? WHERE id = ?",
    )
    .bind(error)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(chunk_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn chunk_meta(
    pool: &SqlitePool,
    chunk_id: i64,
) -> Result<Option<(String, String, String)>> {
    let row =
        sqlx::query("SELECT session_id, message_id, platform FROM embedding_chunks WHERE id = ?")
            .bind(chunk_id)
            .fetch_optional(pool)
            .await?;
    Ok(match row {
        Some(row) => Some((
            row.try_get("session_id")?,
            row.try_get("message_id")?,
            row.try_get("platform")?,
        )),
        None => None,
    })
}

pub async fn count_chunks(
    pool: &SqlitePool,
    identity: &BackendIdentity,
    status: &str,
) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM embedding_chunks WHERE backend_id = ? AND model_id = ? AND status = ?",
    )
    .bind(&identity.backend_id)
    .bind(&identity.model_id)
    .bind(status)
    .fetch_one(pool)
    .await?)
}

pub async fn semantic_session_scores(
    pool: &SqlitePool,
    query: &SearchQuery,
    identity: &BackendIdentity,
    embedding: &[f32],
    top_k: i64,
) -> Result<Vec<(String, f32)>> {
    let mut vector = embedding.to_vec();
    if vector.len() < 640 {
        vector.resize(640, 0.0);
    } else if vector.len() > 640 {
        vector.truncate(640);
    }
    let bytes = f32_slice_as_bytes(&vector);
    let timestamp = timestamp::expression("s.updated_at");
    let sql = format!(
        "SELECT v.session_id AS session_id, MIN(v.distance) AS distance
         FROM embedding_vec v
         INNER JOIN embedding_chunks c ON c.id = v.chunk_id
         INNER JOIN sessions s ON s.id = v.session_id
         WHERE c.backend_id = ? AND c.model_id = ? AND c.status = 'ready'
           AND v.embedding MATCH ?
           AND k = ?
           AND (? IS NULL OR s.platform = ?)
           AND (? IS NULL OR ({timestamp}) >= CAST(? AS REAL))
           AND (? IS NULL OR ({timestamp}) <= CAST(? AS REAL))
         GROUP BY v.session_id
         ORDER BY distance ASC
         LIMIT ?"
    );
    let rows = sqlx::query(&sql)
        .bind(&identity.backend_id)
        .bind(&identity.model_id)
        .bind(bytes)
        .bind(top_k)
        .bind(&query.platform)
        .bind(&query.platform)
        .bind(&query.date_from)
        .bind(&query.date_from)
        .bind(&query.date_to)
        .bind(&query.date_to)
        .bind(top_k)
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let session_id: String = row.try_get("session_id").ok()?;
            let distance: f64 = row.try_get("distance").ok()?;
            let score = (1.0 - distance as f32).max(0.0);
            Some((session_id, score))
        })
        .collect())
}

pub async fn semantic_session_hits(
    pool: &SqlitePool,
    session_id: &str,
    identity: &BackendIdentity,
    embedding: &[f32],
    limit: i64,
) -> Result<Vec<SessionSearchHit>> {
    let mut vector = embedding.to_vec();
    if vector.len() < 640 {
        vector.resize(640, 0.0);
    } else if vector.len() > 640 {
        vector.truncate(640);
    }
    let bytes = f32_slice_as_bytes(&vector);
    let rows = sqlx::query(
        "SELECT v.chunk_id AS chunk_id, v.message_id AS message_id, v.distance AS distance, c.text AS text, m.seq AS seq
         FROM embedding_vec v
         INNER JOIN embedding_chunks c ON c.id = v.chunk_id
         INNER JOIN messages m ON m.id = v.message_id
         WHERE v.session_id = ?
           AND c.backend_id = ?
           AND c.model_id = ?
           AND c.status = 'ready'
           AND v.embedding MATCH ?
           AND k = ?
         ORDER BY distance ASC
         LIMIT ?",
    )
    .bind(session_id)
    .bind(&identity.backend_id)
    .bind(&identity.model_id)
    .bind(bytes)
    .bind(limit)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let message_id: String = row.try_get("message_id").ok()?;
            let seq: i64 = row.try_get("seq").ok()?;
            let distance: f64 = row.try_get("distance").ok()?;
            let text: String = row.try_get("text").ok()?;
            let chunk_id: i64 = row.try_get("chunk_id").ok()?;
            Some(SessionSearchHit {
                message_id,
                seq,
                field: SearchHitField::Semantic,
                count: 1,
                score: Some((1.0 - distance as f32).max(0.0)),
                snippet: Some(truncate_snippet(&text, 180)),
                chunk_id: Some(chunk_id),
            })
        })
        .collect())
}

pub fn reciprocal_rank_fusion(
    keyword: &[(String, f32)],
    semantic: &[(String, f32)],
    k: f32,
) -> Vec<(String, f32)> {
    let mut scores: HashMap<String, f32> = HashMap::new();
    for (rank, (id, _)) in keyword.iter().enumerate() {
        *scores.entry(id.clone()).or_default() += 1.0 / (k + rank as f32 + 1.0);
    }
    for (rank, (id, _)) in semantic.iter().enumerate() {
        *scores.entry(id.clone()).or_default() += 1.0 / (k + rank as f32 + 1.0);
    }
    let mut merged = scores.into_iter().collect::<Vec<_>>();
    merged.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    merged
}

pub async fn summaries_by_ids(pool: &SqlitePool, ids: &[String]) -> Result<Vec<SessionSummary>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut map = HashMap::new();
    for id in ids {
        if let Some(summary) = sqlx::query(
            "SELECT id, platform, platform_session_id, title, created_at, updated_at, imported_at FROM sessions WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?
        {
            map.insert(id.clone(), database::sessions::summary_from_row(summary));
        }
    }
    Ok(ids.iter().filter_map(|id| map.remove(id)).collect())
}

fn f32_slice_as_bytes(values: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn truncate_snippet(text: &str, max_chars: usize) -> String {
    let compact = text.replace(['\r', '\n'], " ");
    let mut out = String::new();
    for (index, ch) in compact.chars().enumerate() {
        if index >= max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrf_prefers_overlap() {
        let keyword = vec![("a".into(), 1.0), ("b".into(), 0.5)];
        let semantic = vec![("b".into(), 1.0), ("c".into(), 0.5)];
        let merged = reciprocal_rank_fusion(&keyword, &semantic, 60.0);
        assert_eq!(merged[0].0, "b");
    }
}
