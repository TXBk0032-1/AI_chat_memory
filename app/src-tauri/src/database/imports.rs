use sqlx::SqlitePool;

use crate::{
    error::Result,
    models::NormalizedSession,
    sync::store::{SyncStore, current_time_millis, snapshot_from_normalized_session},
};

/// 一次导入的新建/更新计数。`inserted` 为首次写入的会话数，`updated` 为命中
/// 已存在会话（upsert 覆盖）的数量。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportCounts {
    pub inserted: usize,
    pub updated: usize,
}

/// 导入并返回处理总数（inserted + updated）。历史调用点只关心总数，签名保持不变。
pub async fn import_sessions(
    pool: &SqlitePool,
    sessions: &[NormalizedSession],
    record_sync: bool,
) -> Result<usize> {
    let counts = import_sessions_counted(pool, sessions, record_sync).await?;
    Ok(counts.inserted + counts.updated)
}

/// 导入并区分新建/更新计数。
pub async fn import_sessions_counted(
    pool: &SqlitePool,
    sessions: &[NormalizedSession],
    record_sync: bool,
) -> Result<ImportCounts> {
    let store = SyncStore::new(pool.clone());
    let now_ms = current_time_millis();
    let mut counts = ImportCounts::default();
    let mut processed = 0usize;
    // Commit each session in its own transaction so a large import does not
    // hold a single write lock across every session (blocking other writers
    // such as cloud merge or maintenance). A failure rolls back only that
    // session and surfaces the count already imported so the caller can
    // report partial progress.
    for session in sessions {
        match import_one_session(pool, &store, session, record_sync, now_ms).await {
            Ok(is_new) => {
                if is_new {
                    counts.inserted += 1;
                } else {
                    counts.updated += 1;
                }
                processed += 1;
            }
            Err(error) => {
                if processed > 0 {
                    tracing::warn!(
                        imported = processed,
                        remaining = sessions.len() - processed,
                        %error,
                        "import_sessions stopped after a partial failure"
                    );
                }
                return Err(error);
            }
        }
    }
    Ok(counts)
}

async fn import_one_session(
    pool: &SqlitePool,
    store: &SyncStore,
    session: &NormalizedSession,
    record_sync: bool,
    now_ms: i64,
) -> Result<bool> {
    let mut tx = pool.begin().await?;
    let record_sync = if record_sync {
        SyncStore::lock_device_state_in(&mut tx).await?.is_some()
    } else {
        false
    };
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT id FROM sessions WHERE platform = ? AND platform_session_id = ?",
    )
    .bind(&session.platform)
    .bind(&session.platform_session_id)
    .fetch_optional(&mut *tx)
    .await?;
    let id = existing.clone().unwrap_or_else(|| session.id.clone());
    let is_new = existing.is_none();
    sqlx::query("INSERT INTO sessions (id, platform, platform_session_id, title, created_at, updated_at, imported_at, raw_data, project, parent_platform_session_id, agent_label) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(platform, platform_session_id) DO UPDATE SET title=excluded.title, created_at=excluded.created_at, updated_at=excluded.updated_at, imported_at=excluded.imported_at, raw_data=excluded.raw_data, project=excluded.project, parent_platform_session_id=excluded.parent_platform_session_id, agent_label=excluded.agent_label")
        .bind(&id).bind(&session.platform).bind(&session.platform_session_id).bind(&session.title)
        .bind(&session.created_at).bind(&session.updated_at).bind(&session.imported_at).bind(serde_json::to_string(&session.raw_data)?)
        .bind(&session.project).bind(&session.parent_platform_session_id).bind(&session.agent_label)
        .execute(&mut *tx).await?;
    sqlx::query("DELETE FROM messages WHERE session_id = ?")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    let has_chunks_table: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'embedding_chunks')",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap_or(false);
    if has_chunks_table {
        let chunk_ids: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM embedding_chunks WHERE session_id = ?")
                .bind(&id)
                .fetch_all(&mut *tx)
                .await?;
        super::maintenance::delete_embedding_vectors_in(&mut tx, &chunk_ids).await?;
        sqlx::query("DELETE FROM embedding_chunks WHERE session_id = ?")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
    }
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
    sqlx::query("INSERT INTO session_fts(rowid, session_id, title, content) VALUES (?, ?, ?, ?)")
        .bind(fts_rowid)
        .bind(&id)
        .bind(&session.title)
        .bind(content)
        .execute(&mut *tx)
        .await?;
    if record_sync {
        store
            .queue_local_upsert_in(&mut tx, snapshot_from_normalized_session(session), now_ms)
            .await?;
    }
    tx.commit().await?;
    Ok(is_new)
}
