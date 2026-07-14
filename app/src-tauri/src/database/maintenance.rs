use sqlx::SqlitePool;

use crate::error::{AppError, Result};

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
