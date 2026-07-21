use sqlx::{Row, SqliteConnection, SqlitePool};

use crate::{
    error::Result,
    models::{SearchQuery, SessionSummary},
};

use super::timestamp;

pub async fn search(pool: &SqlitePool, query: &SearchQuery) -> Result<Vec<SessionSummary>> {
    let mut connection = pool.acquire().await?;
    search_on(&mut connection, query).await
}

pub async fn search_and_count(
    pool: &SqlitePool,
    query: &SearchQuery,
) -> Result<(Vec<SessionSummary>, i64)> {
    let mut tx = pool.begin().await?;
    let total = count_on(&mut tx, query).await?;
    let sessions = search_on(&mut tx, query).await?;
    tx.commit().await?;
    Ok((sessions, total))
}

async fn search_on(
    connection: &mut SqliteConnection,
    query: &SearchQuery,
) -> Result<Vec<SessionSummary>> {
    if let Some(fts_query) = fts_query(query) {
        return search_fts(connection, query, &fts_query).await;
    }
    search_like(connection, query).await
}

async fn search_fts(
    connection: &mut SqliteConnection,
    query: &SearchQuery,
    fts_query: &str,
) -> Result<Vec<SessionSummary>> {
    let timestamp = timestamp::expression("s.updated_at");
    let sql = format!(
        "SELECT s.id, s.platform, s.platform_session_id, s.title, s.created_at, s.updated_at, s.imported_at
         FROM session_fts
         INNER JOIN sessions s ON s.id = session_fts.session_id
         WHERE session_fts MATCH ?
           AND (? IS NULL OR s.platform = ?)
           AND (? IS NULL OR ({timestamp}) >= CAST(? AS REAL))
           AND (? IS NULL OR ({timestamp}) <= CAST(? AS REAL))
         ORDER BY bm25(session_fts, 0.0, 2.0, 1.0) ASC, ({timestamp}) DESC, s.id ASC
         LIMIT ? OFFSET ?"
    );
    let rows = sqlx::query(&sql)
        .bind(fts_query)
        .bind(&query.platform)
        .bind(&query.platform)
        .bind(&query.date_from)
        .bind(&query.date_from)
        .bind(&query.date_to)
        .bind(&query.date_to)
        .bind(query.limit.unwrap_or(500).clamp(1, 1000))
        .bind(query.offset.unwrap_or(0).max(0))
        .fetch_all(&mut *connection)
        .await?;
    Ok(rows.into_iter().map(summary_from_row).collect())
}

async fn search_like(
    connection: &mut SqliteConnection,
    query: &SearchQuery,
) -> Result<Vec<SessionSummary>> {
    let timestamp = timestamp::expression("s.updated_at");
    let like_query = query.q.as_deref().map(escape_like);
    let sql = format!(
        "SELECT s.id, s.platform, s.platform_session_id, s.title, s.created_at, s.updated_at, s.imported_at FROM sessions s WHERE (? IS NULL OR s.platform = ?) AND (? IS NULL OR ({timestamp}) >= CAST(? AS REAL)) AND (? IS NULL OR ({timestamp}) <= CAST(? AS REAL)) AND (? IS NULL OR s.title LIKE '%' || ? || '%' ESCAPE '\\' OR EXISTS (SELECT 1 FROM messages m WHERE m.session_id=s.id AND m.content LIKE '%' || ? || '%' ESCAPE '\\')) ORDER BY ({timestamp}) DESC, s.id ASC LIMIT ? OFFSET ?"
    );
    let rows = sqlx::query(&sql)
        .bind(&query.platform)
        .bind(&query.platform)
        .bind(&query.date_from)
        .bind(&query.date_from)
        .bind(&query.date_to)
        .bind(&query.date_to)
        .bind(&like_query)
        .bind(&like_query)
        .bind(&like_query)
        .bind(query.limit.unwrap_or(500).clamp(1, 1000))
        .bind(query.offset.unwrap_or(0).max(0))
        .fetch_all(&mut *connection)
        .await?;
    Ok(rows.into_iter().map(summary_from_row).collect())
}

async fn count_on(connection: &mut SqliteConnection, query: &SearchQuery) -> Result<i64> {
    if let Some(fts_query) = fts_query(query) {
        return count_fts(connection, query, &fts_query).await;
    }
    count_like(connection, query).await
}

async fn count_fts(
    connection: &mut SqliteConnection,
    query: &SearchQuery,
    fts_query: &str,
) -> Result<i64> {
    let timestamp = timestamp::expression("s.updated_at");
    let sql = format!(
        "SELECT COUNT(*)
         FROM session_fts
         INNER JOIN sessions s ON s.id = session_fts.session_id
         WHERE session_fts MATCH ?
           AND (? IS NULL OR s.platform = ?)
           AND (? IS NULL OR ({timestamp}) >= CAST(? AS REAL))
           AND (? IS NULL OR ({timestamp}) <= CAST(? AS REAL))"
    );
    Ok(sqlx::query_scalar(&sql)
        .bind(fts_query)
        .bind(&query.platform)
        .bind(&query.platform)
        .bind(&query.date_from)
        .bind(&query.date_from)
        .bind(&query.date_to)
        .bind(&query.date_to)
        .fetch_one(&mut *connection)
        .await?)
}

async fn count_like(connection: &mut SqliteConnection, query: &SearchQuery) -> Result<i64> {
    let timestamp = timestamp::expression("s.updated_at");
    let like_query = query.q.as_deref().map(escape_like);
    let sql = format!(
        "SELECT COUNT(*) FROM sessions s WHERE (? IS NULL OR s.platform = ?) AND (? IS NULL OR ({timestamp}) >= CAST(? AS REAL)) AND (? IS NULL OR ({timestamp}) <= CAST(? AS REAL)) AND (? IS NULL OR s.title LIKE '%' || ? || '%' ESCAPE '\\' OR EXISTS (SELECT 1 FROM messages m WHERE m.session_id=s.id AND m.content LIKE '%' || ? || '%' ESCAPE '\\'))"
    );
    Ok(sqlx::query_scalar(&sql)
        .bind(&query.platform)
        .bind(&query.platform)
        .bind(&query.date_from)
        .bind(&query.date_from)
        .bind(&query.date_to)
        .bind(&query.date_to)
        .bind(&like_query)
        .bind(&like_query)
        .bind(&like_query)
        .fetch_one(&mut *connection)
        .await?)
}

fn fts_query(query: &SearchQuery) -> Option<String> {
    let value = query.q.as_deref()?.trim();
    (value.chars().count() >= 3).then(|| format!("\"{}\"", value.replace('"', "\"\"")))
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn search_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE sessions (id TEXT PRIMARY KEY, platform TEXT NOT NULL, platform_session_id TEXT NOT NULL, title TEXT, created_at TEXT, updated_at TEXT, imported_at TEXT, raw_data TEXT, UNIQUE(platform, platform_session_id)); CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, role TEXT NOT NULL, content TEXT, metadata TEXT, created_at TEXT, seq INTEGER); CREATE VIRTUAL TABLE session_fts USING fts5(session_id UNINDEXED, title, content, tokenize = 'trigram');")
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    async fn insert_search_document(
        pool: &SqlitePool,
        id: &str,
        title: &str,
        content: &str,
        updated_at: &str,
    ) {
        sqlx::query("INSERT INTO sessions (id, platform, platform_session_id, title, updated_at) VALUES (?, 'test', ?, ?, ?)")
            .bind(id)
            .bind(id)
            .bind(title)
            .bind(updated_at)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO messages (id, session_id, role, content, metadata, seq) VALUES (?, ?, 'user', ?, '{}', 0)")
            .bind(format!("m-{id}"))
            .bind(id)
            .bind(content)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO session_fts(session_id, title, content) VALUES (?, ?, ?)")
            .bind(id)
            .bind(title)
            .bind(content)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn ranks_fts_results_with_title_weighted_two_to_one() {
        let pool = search_pool().await;
        insert_search_document(&pool, "title-hit", "searchable topic", "other", "1").await;
        insert_search_document(&pool, "content-hit", "other", "searchable topic", "2").await;

        let (rows, total) = search_and_count(
            &pool,
            &SearchQuery {
                q: Some("searchable".into()),
                ..SearchQuery::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(total, 2);
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["title-hit", "content-hit"]
        );
    }

    #[tokio::test]
    async fn keeps_short_queries_on_literal_like_search() {
        let pool = search_pool().await;
        insert_search_document(&pool, "short", "AI 记录", "other", "1").await;

        let rows = search(
            &pool,
            &SearchQuery {
                q: Some("AI".into()),
                ..SearchQuery::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "short");
    }

    #[tokio::test]
    async fn treats_fts_syntax_as_literal_text() {
        let pool = search_pool().await;
        insert_search_document(&pool, "literal", "say \"hello\" now", "other", "1").await;
        insert_search_document(&pool, "unquoted", "say hello now", "other", "2").await;

        let rows = search(
            &pool,
            &SearchQuery {
                q: Some("\"hello\"".into()),
                ..SearchQuery::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "literal");
    }

    #[tokio::test]
    async fn treats_short_like_wildcards_as_literal_text() {
        let pool = search_pool().await;
        insert_search_document(&pool, "literal", "100% complete", "other", "1").await;
        insert_search_document(&pool, "unrelated", "complete", "other", "2").await;

        let rows = search(
            &pool,
            &SearchQuery {
                q: Some("%".into()),
                ..SearchQuery::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "literal");
    }

    #[tokio::test]
    async fn stabilizes_like_pagination_by_session_id() {
        let pool = search_pool().await;
        insert_search_document(&pool, "b", "AI second", "other", "1").await;
        insert_search_document(&pool, "a", "AI first", "other", "1").await;

        let rows = search(
            &pool,
            &SearchQuery {
                q: Some("AI".into()),
                limit: Some(1),
                ..SearchQuery::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(rows[0].id, "a");
    }
}
