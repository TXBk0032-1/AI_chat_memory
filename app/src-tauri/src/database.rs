use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::path::Path;

use crate::{
    error::{AppError, Result},
    models::{Message, NormalizedSession, SearchQuery, SessionDetail, SessionSummary},
};

pub async fn connect(path: &Path) -> Result<SqlitePool> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, platform TEXT NOT NULL, platform_session_id TEXT NOT NULL, title TEXT, created_at TEXT, updated_at TEXT, imported_at TEXT DEFAULT CURRENT_TIMESTAMP, raw_data TEXT, UNIQUE(platform, platform_session_id));").execute(&pool).await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS messages (id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, role TEXT NOT NULL, content TEXT, metadata TEXT, created_at TEXT, seq INTEGER);").execute(&pool).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);")
        .execute(&pool)
        .await?;
    Ok(pool)
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
    let rows = sqlx::query("SELECT s.id, s.platform, s.platform_session_id, s.title, s.created_at, s.updated_at, s.imported_at FROM sessions s WHERE (? IS NULL OR s.platform = ?) AND (? IS NULL OR s.updated_at >= ?) AND (? IS NULL OR s.updated_at <= ?) AND (? IS NULL OR s.title LIKE '%' || ? || '%' OR EXISTS (SELECT 1 FROM messages m WHERE m.session_id=s.id AND m.content LIKE '%' || ? || '%')) ORDER BY s.updated_at DESC LIMIT ? OFFSET ?")
        .bind(&query.platform).bind(&query.platform).bind(&query.date_from).bind(&query.date_from)
        .bind(&query.date_to).bind(&query.date_to).bind(&query.q).bind(&query.q).bind(&query.q)
        .bind(query.limit.unwrap_or(500).clamp(1, 1000)).bind(query.offset.unwrap_or(0).max(0)).fetch_all(pool).await?;
    Ok(rows.into_iter().map(summary_from_row).collect())
}

pub async fn get_session(pool: &SqlitePool, id: &str) -> Result<SessionDetail> {
    let row = sqlx::query("SELECT id, platform, platform_session_id, title, created_at, updated_at, imported_at, raw_data FROM sessions WHERE id = ?")
        .bind(id).fetch_optional(pool).await?.ok_or_else(|| AppError::NotFound("session".into()))?;
    let raw_data = row
        .try_get::<Option<String>, _>("raw_data")?
        .map(|s| serde_json::from_str(&s))
        .transpose()?;
    let summary = summary_from_row(row);
    let rows = sqlx::query("SELECT id, session_id, role, content, metadata, created_at, seq FROM messages WHERE session_id = ? ORDER BY seq")
        .bind(id).fetch_all(pool).await?;
    let messages = rows
        .into_iter()
        .map(|r| -> Result<Message> {
            Ok(Message {
                id: r.try_get("id")?,
                session_id: r.try_get("session_id")?,
                role: r.try_get("role")?,
                content: r.try_get("content")?,
                metadata: serde_json::from_str(&r.try_get::<String, _>("metadata")?)?,
                created_at: r.try_get("created_at")?,
                seq: r.try_get("seq")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(SessionDetail {
        summary,
        messages,
        raw_data,
    })
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
    Ok(
        sqlx::query_scalar("SELECT MAX(updated_at) FROM sessions WHERE platform = ?")
            .bind(platform)
            .fetch_one(pool)
            .await?,
    )
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
}
