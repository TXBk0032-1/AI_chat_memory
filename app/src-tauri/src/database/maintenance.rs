use sqlx::SqlitePool;

use crate::error::{AppError, Result};

pub async fn delete_session(pool: &SqlitePool, id: &str) -> Result<()> {
    let mut tx = pool.begin().await?;
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
