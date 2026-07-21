use sqlx::SqlitePool;

use crate::{error::Result, models::NormalizedSession};

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
        sqlx::query(
            "INSERT INTO session_fts_ids(session_id) VALUES (?)
             ON CONFLICT(session_id) DO NOTHING",
        )
        .bind(&id)
        .execute(&mut *tx)
        .await?;
        let fts_rowid: i64 =
            sqlx::query_scalar("SELECT fts_rowid FROM session_fts_ids WHERE session_id = ?")
                .bind(&id)
                .fetch_one(&mut *tx)
                .await?;
        sqlx::query("DELETE FROM session_fts WHERE rowid = ?")
            .bind(fts_rowid)
            .execute(&mut *tx)
            .await?;
        let content = session
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        sqlx::query(
            "INSERT INTO session_fts(rowid, session_id, title, content) VALUES (?, ?, ?, ?)",
        )
        .bind(fts_rowid)
        .bind(&id)
        .bind(&session.title)
        .bind(content)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(sessions.len())
}
