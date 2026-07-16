use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::path::Path;
use std::sync::Once;

use crate::error::Result;

use super::timestamp;

static SQLITE_VEC_INIT: Once = Once::new();

pub fn register_sqlite_vec() {
    SQLITE_VEC_INIT.call_once(|| {
        unsafe {
            // SAFETY: sqlite-vec is a trusted, statically linked extension compiled into this binary.
            type SqliteEntry = unsafe extern "C" fn(
                *mut libsqlite3_sys::sqlite3,
                *mut *mut i8,
                *const libsqlite3_sys::sqlite3_api_routines,
            ) -> i32;
            libsqlite3_sys::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                SqliteEntry,
            >(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }
        tracing::info!("sqlite-vec extension registered");
    });
}

pub async fn connect(path: &Path) -> Result<SqlitePool> {
    register_sqlite_vec();
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
    // Full-table timestamp rewrites are expensive on large archives; only do them
    // once for small/fresh databases so startup stays interactive.
    maybe_normalize_stored_timestamps(&pool).await?;
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
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS embedding_chunks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            message_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            platform TEXT NOT NULL,
            chunk_index INTEGER NOT NULL,
            role TEXT NOT NULL,
            text TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            backend_id TEXT NOT NULL,
            model_id TEXT NOT NULL,
            dim INTEGER NOT NULL,
            status TEXT NOT NULL,
            error TEXT,
            updated_at TEXT NOT NULL,
            UNIQUE(message_id, chunk_index, backend_id, model_id)
        );",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_embedding_chunks_session ON embedding_chunks(session_id);",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_embedding_chunks_status ON embedding_chunks(status, backend_id, model_id);",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE VIRTUAL TABLE IF NOT EXISTS embedding_vec USING vec0(
            chunk_id INTEGER PRIMARY KEY,
            embedding float[640] distance_metric=cosine,
            +session_id TEXT,
            +message_id TEXT,
            +platform TEXT
        );",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn maybe_normalize_stored_timestamps(pool: &SqlitePool) -> Result<()> {
    let message_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    // Existing large installs already went through the one-time rewrite; skip the
    // multi-second full-table scan on every launch.
    if message_count > 2_000 {
        tracing::info!(
            message_count,
            "skipping startup timestamp normalize for large database"
        );
        return Ok(());
    }
    normalize_stored_timestamps(pool).await
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
    register_sqlite_vec();
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    #[tokio::test]
    async fn registers_sqlite_vec_and_creates_vector_table() {
        register_sqlite_vec();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        initialize_schema(&pool).await.unwrap();
        let version: String = sqlx::query_scalar("SELECT vec_version()")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(version.starts_with('v'), "{version}");
    }
}
