use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::path::Path;

use crate::error::Result;

use super::timestamp;

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
    initialize_schema(&pool).await?;
    normalize_stored_timestamps(&pool).await?;
    Ok(pool)
}

async fn initialize_schema(pool: &SqlitePool) -> Result<()> {
    sqlx::query("CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, platform TEXT NOT NULL, platform_session_id TEXT NOT NULL, title TEXT, created_at TEXT, updated_at TEXT, imported_at TEXT DEFAULT CURRENT_TIMESTAMP, raw_data TEXT, UNIQUE(platform, platform_session_id));").execute(pool).await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS messages (id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, role TEXT NOT NULL, content TEXT, metadata TEXT, created_at TEXT, seq INTEGER);").execute(pool).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);")
        .execute(pool)
        .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_messages_session_seq ON messages(session_id, seq);",
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub(super) async fn normalize_stored_timestamps(pool: &SqlitePool) -> Result<()> {
    for (table, column) in [
        ("sessions", "created_at"),
        ("sessions", "updated_at"),
        ("messages", "created_at"),
    ] {
        let expression = timestamp::expression(column);
        let sql = format!(
            "UPDATE {table} SET {column} = CAST(({expression}) AS TEXT) WHERE {column} IS NOT NULL AND ({expression}) IS NOT NULL"
        );
        sqlx::query(&sql).execute(pool).await?;
    }
    Ok(())
}

pub async fn copy_database(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let options = SqliteConnectOptions::new()
        .filename(source)
        .read_only(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    let result = sqlx::query("VACUUM INTO ?")
        .bind(destination.to_string_lossy().as_ref())
        .execute(&pool)
        .await;
    pool.close().await;
    result?;
    Ok(())
}
