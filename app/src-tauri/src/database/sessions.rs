use sqlx::{Row, SqlitePool};

use crate::{
    error::Result,
    models::{SearchQuery, SessionSummary},
};

use super::timestamp;

pub async fn search(pool: &SqlitePool, query: &SearchQuery) -> Result<Vec<SessionSummary>> {
    let timestamp = timestamp::expression("s.updated_at");
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
    let timestamp = timestamp::expression("s.updated_at");
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

pub(crate) fn summary_from_row(row: sqlx::sqlite::SqliteRow) -> SessionSummary {
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

