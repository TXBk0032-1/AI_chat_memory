use sqlx::SqlitePool;

use crate::{
    error::{AppError, Result},
    sync::{
        store::{SyncStore, current_time_millis},
        types::EntityKey,
    },
};

pub async fn delete_session(pool: &SqlitePool, id: &str, record_sync: bool) -> Result<()> {
    let store = SyncStore::new(pool.clone());
    let mut tx = pool.begin().await?;
    let record_sync = if record_sync {
        SyncStore::lock_device_state_in(&mut tx).await?.is_some()
    } else {
        false
    };
    let now_ms = current_time_millis();
    let key = sqlx::query_as::<_, (String, String)>(
        "SELECT platform, platform_session_id FROM sessions WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .map(|(platform, platform_session_id)| EntityKey {
        platform,
        platform_session_id,
    })
    .ok_or_else(|| AppError::NotFound("session".into()))?;
    let fts_rowid: Option<i64> =
        sqlx::query_scalar("SELECT fts_rowid FROM session_fts_ids WHERE session_id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some(fts_rowid) = fts_rowid {
        sqlx::query("DELETE FROM session_fts WHERE rowid = ?")
            .bind(fts_rowid)
            .execute(&mut *tx)
            .await?;
    }
    let has_chunks_table: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'embedding_chunks')",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap_or(false);

    if has_chunks_table {
        let chunk_ids: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM embedding_chunks WHERE session_id = ?")
                .bind(id)
                .fetch_all(&mut *tx)
                .await?;
        delete_embedding_vectors_in(&mut tx, &chunk_ids).await?;
        sqlx::query("DELETE FROM embedding_chunks WHERE session_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    let result = sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("session".into()));
    }
    if record_sync {
        store.queue_local_delete_in(&mut tx, key, now_ms).await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn sync_status(pool: &SqlitePool, platform: &str) -> Result<Option<String>> {
    let expr = crate::database::timestamp::expression("updated_at");
    // Return the same normalized epoch-seconds value that drives the ordering
    // so clients always receive a stable format, regardless of whether the
    // stored updated_at is an ISO string, millisecond or second epoch.
    let query = format!(
        "SELECT CAST(({expr}) AS TEXT) FROM sessions WHERE platform = ? AND updated_at IS NOT NULL AND trim(updated_at) != '' ORDER BY ({expr}) DESC LIMIT 1"
    );
    let value = sqlx::query_scalar::<_, Option<String>>(&query)
        .bind(platform)
        .fetch_optional(pool)
        .await?
        .flatten();
    Ok(value)
}

/// Deletes the embedding vectors for `chunk_ids` with batched
/// `DELETE ... WHERE chunk_id IN (...)` statements instead of one statement per
/// chunk. Errors propagate so the surrounding transaction can roll back.
pub async fn delete_embedding_vectors_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    chunk_ids: &[i64],
) -> Result<()> {
    const BATCH_SIZE: usize = 64;
    for batch in chunk_ids.chunks(BATCH_SIZE) {
        let placeholders = std::iter::repeat_n("?", batch.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("DELETE FROM embedding_vec WHERE chunk_id IN ({placeholders})");
        let mut query = sqlx::query(&sql);
        for chunk_id in batch {
            query = query.bind(chunk_id);
        }
        query.execute(&mut **tx).await?;
    }
    Ok(())
}
