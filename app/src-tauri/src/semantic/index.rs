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
    pub session_id: String,
    pub message_id: String,
    pub platform: String,
}

pub async fn queue_session_chunks(
    pool: &SqlitePool,
    session_id: &str,
    identity: &BackendIdentity,
) -> Result<usize> {
    queue_session_chunks_with_force(pool, session_id, identity, false).await
}

pub async fn queue_session_chunks_with_force(
    pool: &SqlitePool,
    session_id: &str,
    identity: &BackendIdentity,
    force: bool,
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
            if !force && content_hash == chunk.content_hash && status == "ready" {
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

#[allow(dead_code)]
pub async fn queue_all_sessions(pool: &SqlitePool, identity: &BackendIdentity) -> Result<usize> {
    queue_all_sessions_with_progress(pool, identity, false, None::<fn(usize, usize, usize)>).await
}

pub async fn queue_all_sessions_with_progress<F>(
    pool: &SqlitePool,
    identity: &BackendIdentity,
    force: bool,
    mut on_progress: Option<F>,
) -> Result<usize>
where
    F: FnMut(usize, usize, usize),
{
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM sessions")
        .fetch_all(pool)
        .await?;
    let total_sessions = ids.len();
    let mut total = 0usize;
    for (index, id) in ids.into_iter().enumerate() {
        total += queue_session_chunks_with_force(pool, &id, identity, force).await?;
        if let Some(callback) = on_progress.as_mut() {
            callback(index + 1, total_sessions, total);
        }
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
        "SELECT id, text, session_id, message_id, platform FROM embedding_chunks WHERE status = 'pending' AND backend_id = ? AND model_id = ? ORDER BY id ASC LIMIT ?",
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
            session_id: row.get("session_id"),
            message_id: row.get("message_id"),
            platform: row.get("platform"),
        })
        .collect())
}

#[allow(dead_code)]
pub async fn mark_chunk_ready(
    pool: &SqlitePool,
    chunk_id: i64,
    identity: &BackendIdentity,
    session_id: &str,
    message_id: &str,
    platform: &str,
    embedding: &[f32],
) -> Result<()> {
    mark_chunks_ready(
        pool,
        identity,
        &[(
            chunk_id,
            session_id,
            message_id,
            platform,
            embedding.to_vec(),
        )],
    )
    .await
}

pub async fn mark_chunks_ready(
    pool: &SqlitePool,
    identity: &BackendIdentity,
    items: &[(i64, &str, &str, &str, Vec<f32>)],
) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let total_started = std::time::Instant::now();

    let prepare_started = std::time::Instant::now();
    let prepared = items
        .iter()
        .map(|(chunk_id, session_id, message_id, platform, embedding)| {
            let mut vector = embedding.clone();
            let dim = identity.dimensions.max(1);
            if vector.len() < dim {
                vector.resize(dim, 0.0);
            } else if vector.len() > dim {
                vector.truncate(dim);
            }
            let bytes = f32_slice_as_bytes(&vector);
            (*chunk_id, *session_id, *message_id, *platform, bytes)
        })
        .collect::<Vec<_>>();
    let prepare_ms = prepare_started.elapsed().as_millis();

    let mut tx = pool.begin().await?;
    let now = chrono::Utc::now().to_rfc3339();

    // Fresh pending chunks usually have no prior vector row. Only delete when
    // some already exist (reindex/stale rewrite), to avoid expensive no-op vec0 DELETEs.
    let delete_started = std::time::Instant::now();
    let mut existing_ids = Vec::new();
    {
        let mut exists_sql = String::from("SELECT chunk_id FROM embedding_vec WHERE chunk_id IN (");
        for (i, (chunk_id, _, _, _, _)) in prepared.iter().enumerate() {
            if i > 0 {
                exists_sql.push(',');
            }
            exists_sql.push_str(&chunk_id.to_string());
        }
        exists_sql.push(')');
        let rows = sqlx::query(&exists_sql).fetch_all(&mut *tx).await?;
        for row in rows {
            existing_ids.push(row.get::<i64, _>("chunk_id"));
        }
    }
    if !existing_ids.is_empty() {
        let mut delete_sql = String::from("DELETE FROM embedding_vec WHERE chunk_id IN (");
        for (i, chunk_id) in existing_ids.iter().enumerate() {
            if i > 0 {
                delete_sql.push(',');
            }
            delete_sql.push_str(&chunk_id.to_string());
        }
        delete_sql.push(')');
        let _ = sqlx::query(&delete_sql).execute(&mut *tx).await;
    }
    let delete_ms = delete_started.elapsed().as_millis();

    let mut insert_ms = 0u128;
    let mut update_ms = 0u128;
    for (chunk_id, session_id, message_id, platform, bytes) in &prepared {
        let insert_started = std::time::Instant::now();
        sqlx::query(
            "INSERT INTO embedding_vec(chunk_id, embedding, session_id, message_id, platform) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(chunk_id)
        .bind(bytes)
        .bind(session_id)
        .bind(message_id)
        .bind(platform)
        .execute(&mut *tx)
        .await?;
        insert_ms += insert_started.elapsed().as_millis();

        let update_started = std::time::Instant::now();
        sqlx::query(
            "UPDATE embedding_chunks SET status = 'ready', error = NULL, dim = ?, updated_at = ? WHERE id = ?",
        )
        .bind(identity.dimensions as i64)
        .bind(&now)
        .bind(chunk_id)
        .execute(&mut *tx)
        .await?;
        update_ms += update_started.elapsed().as_millis();
    }
    let commit_started = std::time::Instant::now();
    tx.commit().await?;
    let commit_ms = commit_started.elapsed().as_millis();
    let total_ms = total_started.elapsed().as_millis();
    tracing::info!(
        batch_size = items.len(),
        existing_vectors = existing_ids.len(),
        prepare_ms,
        delete_ms,
        insert_ms,
        update_ms,
        commit_ms,
        total_ms,
        "semantic mark_chunks_ready profile"
    );
    Ok(())
}

#[allow(dead_code)]
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

#[allow(dead_code)]
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
    if top_k <= 0 {
        return Ok(Vec::new());
    }
    let clamped_top_k = top_k.clamp(1, 4096);
    let candidate_k = (clamped_top_k * 8).min(4096);
    let mut vector = embedding.to_vec();
    let dim = identity.dimensions.max(1);
    if vector.len() < dim {
        vector.resize(dim, 0.0);
    } else if vector.len() > dim {
        vector.truncate(dim);
    }
    let bytes = f32_slice_as_bytes(&vector);
    let timestamp = timestamp::expression("s.updated_at");
    let sql = format!(
        "WITH nearest AS MATERIALIZED (
             SELECT session_id, chunk_id, distance
             FROM embedding_vec
             WHERE embedding MATCH ? AND k = ?
               AND chunk_id IN (
                   SELECT c.id
                   FROM embedding_chunks c
                   INNER JOIN sessions s ON s.id = c.session_id
                   WHERE c.backend_id = ? AND c.model_id = ? AND c.status = 'ready'
                     AND (? IS NULL OR s.platform = ?)
                     AND (? IS NULL OR ({timestamp}) >= CAST(? AS REAL))
                     AND (? IS NULL OR ({timestamp}) <= CAST(? AS REAL))
               )
             ORDER BY distance
         )
         SELECT nearest.session_id AS session_id, MIN(nearest.distance) AS distance
         FROM nearest
         GROUP BY nearest.session_id
         ORDER BY distance ASC, session_id ASC
         LIMIT ?"
    );
    let rows = sqlx::query(&sql)
        .bind(bytes)
        .bind(candidate_k)
        .bind(&identity.backend_id)
        .bind(&identity.model_id)
        .bind(&query.platform)
        .bind(&query.platform)
        .bind(&query.date_from)
        .bind(&query.date_from)
        .bind(&query.date_to)
        .bind(&query.date_to)
        .bind(clamped_top_k)
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
    let dim = identity.dimensions.max(1);
    if vector.len() < dim {
        vector.resize(dim, 0.0);
    } else if vector.len() > dim {
        vector.truncate(dim);
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
    use crate::{database::connection, models::EmbeddingBackendKind};
    use sqlx::sqlite::SqlitePoolOptions;

    async fn semantic_scores_pool() -> SqlitePool {
        connection::register_sqlite_vec();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::raw_sql(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY, platform TEXT NOT NULL, updated_at TEXT
            );
             CREATE TABLE embedding_chunks (
                id INTEGER PRIMARY KEY, session_id TEXT NOT NULL,
                backend_id TEXT NOT NULL, model_id TEXT NOT NULL, status TEXT NOT NULL
             );
             CREATE VIRTUAL TABLE embedding_vec USING vec0(
                chunk_id INTEGER PRIMARY KEY,
                embedding float[8] distance_metric=cosine,
                +session_id TEXT
             );",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    fn semantic_scores_identity() -> BackendIdentity {
        BackendIdentity {
            backend: EmbeddingBackendKind::Local,
            backend_id: "local".into(),
            model_id: "test".into(),
            dimensions: 8,
        }
    }

    async fn insert_semantic_score_chunk(
        pool: &SqlitePool,
        chunk_id: i64,
        session_id: &str,
        backend_id: &str,
        model_id: &str,
        status: &str,
        embedding: [f32; 8],
    ) {
        sqlx::query(
            "INSERT INTO embedding_chunks (id, session_id, backend_id, model_id, status)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(chunk_id)
        .bind(session_id)
        .bind(backend_id)
        .bind(model_id)
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO embedding_vec(chunk_id, embedding, session_id) VALUES (?, ?, ?)")
            .bind(chunk_id)
            .bind(f32_slice_as_bytes(&embedding))
            .bind(session_id)
            .execute(pool)
            .await
            .unwrap();
    }

    #[test]
    fn rrf_prefers_overlap() {
        let keyword = vec![("a".into(), 1.0), ("b".into(), 0.5)];
        let semantic = vec![("b".into(), 1.0), ("c".into(), 0.5)];
        let merged = reciprocal_rank_fusion(&keyword, &semantic, 60.0);
        assert_eq!(merged[0].0, "b");
    }

    #[tokio::test]
    async fn semantic_scores_aggregate_vec_knn_results_by_session() {
        let pool = semantic_scores_pool().await;
        sqlx::raw_sql(
            "INSERT INTO sessions VALUES ('s1', 'chatgpt', '2026-01-01'), ('s2', 'claude', '2026-01-02');
             INSERT INTO embedding_chunks VALUES
                (1, 's1', 'local', 'test', 'ready'),
                (2, 's1', 'local', 'test', 'ready'),
                (3, 's2', 'local', 'test', 'ready');",
        )
        .execute(&pool)
        .await
        .unwrap();
        for (chunk_id, session_id, embedding) in [
            (
                1_i64,
                "s1",
                vec![1.0_f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            ),
            (2, "s1", vec![0.8_f32, 0.2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            (3, "s2", vec![0.0_f32, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        ] {
            sqlx::query(
                "INSERT INTO embedding_vec(chunk_id, embedding, session_id) VALUES (?, ?, ?)",
            )
            .bind(chunk_id)
            .bind(f32_slice_as_bytes(&embedding))
            .bind(session_id)
            .execute(&pool)
            .await
            .unwrap();
        }

        let scores = semantic_session_scores(
            &pool,
            &SearchQuery::default(),
            &semantic_scores_identity(),
            &[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            3,
        )
        .await
        .unwrap();

        assert_eq!(scores.len(), 2);
        assert_eq!(scores[0].0, "s1");
        assert!(scores[0].1 > scores[1].1);
    }

    #[tokio::test]
    async fn semantic_scores_filter_before_knn_candidate_limit() {
        let pool = semantic_scores_pool().await;
        sqlx::raw_sql(
            "INSERT INTO sessions VALUES
                ('eligible', 'chatgpt', '200'),
                ('wrong-platform', 'claude', '200'),
                ('too-old', 'chatgpt', '50'),
                ('wrong-backend', 'chatgpt', '200'),
                ('wrong-model', 'chatgpt', '200'),
                ('pending', 'chatgpt', '200');",
        )
        .execute(&pool)
        .await
        .unwrap();
        for (chunk_id, session_id, backend_id, model_id, status) in [
            (1, "wrong-platform", "local", "test", "ready"),
            (2, "too-old", "local", "test", "ready"),
            (3, "wrong-backend", "remote", "test", "ready"),
            (4, "wrong-model", "local", "other", "ready"),
            (5, "pending", "local", "test", "pending"),
            (6, "pending", "local", "test", "pending"),
            (7, "wrong-platform", "local", "test", "ready"),
            (8, "too-old", "local", "test", "ready"),
        ] {
            insert_semantic_score_chunk(
                &pool,
                chunk_id,
                session_id,
                backend_id,
                model_id,
                status,
                [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            )
            .await;
        }
        insert_semantic_score_chunk(
            &pool,
            9,
            "eligible",
            "local",
            "test",
            "ready",
            [0.8, 0.2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        )
        .await;

        let scores = semantic_session_scores(
            &pool,
            &SearchQuery {
                platform: Some("chatgpt".into()),
                date_from: Some("100".into()),
                date_to: Some("300".into()),
                ..SearchQuery::default()
            },
            &semantic_scores_identity(),
            &[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            1,
        )
        .await
        .unwrap();

        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].0, "eligible");
    }

    #[tokio::test]
    async fn semantic_scores_expand_chunk_candidates_before_session_limit() {
        let pool = semantic_scores_pool().await;
        sqlx::raw_sql(
            "INSERT INTO sessions VALUES
                ('dominant', 'chatgpt', '200'),
                ('also-relevant', 'chatgpt', '200');",
        )
        .execute(&pool)
        .await
        .unwrap();
        for (chunk_id, embedding) in [
            (1, [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            (2, [0.99, 0.01, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        ] {
            insert_semantic_score_chunk(
                &pool, chunk_id, "dominant", "local", "test", "ready", embedding,
            )
            .await;
        }
        insert_semantic_score_chunk(
            &pool,
            3,
            "also-relevant",
            "local",
            "test",
            "ready",
            [0.8, 0.2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        )
        .await;

        let scores = semantic_session_scores(
            &pool,
            &SearchQuery::default(),
            &semantic_scores_identity(),
            &[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            2,
        )
        .await
        .unwrap();

        assert_eq!(
            scores.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            ["dominant", "also-relevant"]
        );
    }

    #[tokio::test]
    async fn semantic_scores_sort_equal_distances_by_session_id() {
        let pool = semantic_scores_pool().await;
        sqlx::raw_sql(
            "INSERT INTO sessions VALUES
                ('z-session', 'chatgpt', '200'),
                ('a-session', 'chatgpt', '200');",
        )
        .execute(&pool)
        .await
        .unwrap();
        for (chunk_id, session_id) in [(1, "z-session"), (2, "a-session")] {
            insert_semantic_score_chunk(
                &pool,
                chunk_id,
                session_id,
                "local",
                "test",
                "ready",
                [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            )
            .await;
        }

        let scores = semantic_session_scores(
            &pool,
            &SearchQuery::default(),
            &semantic_scores_identity(),
            &[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            2,
        )
        .await
        .unwrap();

        assert_eq!(
            scores.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            ["a-session", "z-session"]
        );
    }

    #[tokio::test]
    async fn semantic_scores_return_empty_for_non_positive_top_k() {
        let pool = semantic_scores_pool().await;

        for top_k in [0, -1] {
            let scores = semantic_session_scores(
                &pool,
                &SearchQuery::default(),
                &semantic_scores_identity(),
                &[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                top_k,
            )
            .await
            .unwrap();
            assert!(scores.is_empty());
        }
    }
}
