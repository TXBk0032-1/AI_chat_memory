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
    Ok(sqlx::query_scalar(
        "SELECT CAST(MAX(CAST(updated_at AS REAL)) AS TEXT) FROM sessions WHERE platform = ?",
    )
    .bind(platform)
    .fetch_one(pool)
    .await?)
}
