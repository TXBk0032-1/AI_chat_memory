use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::path::Path;
use std::sync::Once;
use std::time::Duration;

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
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;
    // WAL + moderate sync is a better default for large embedding rebuilds.
    sqlx::query("PRAGMA journal_mode = WAL;")
        .execute(&pool)
        .await?;
    sqlx::query("PRAGMA synchronous = NORMAL;")
        .execute(&pool)
        .await?;
    sqlx::query("PRAGMA temp_store = MEMORY;")
        .execute(&pool)
        .await?;
    initialize_schema(&pool).await?;
    // Full-table timestamp rewrites are expensive on large archives; only do them
    // once for small/fresh databases so startup stays interactive.
    maybe_normalize_stored_timestamps(&pool).await?;
    Ok(pool)
}

pub(crate) async fn initialize_schema(pool: &SqlitePool) -> Result<()> {
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
    initialize_session_fts(pool).await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS embedding_chunks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            message_id TEXT NOT NULL,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
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
    ensure_embedding_chunks_foreign_key(pool).await?;
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
        "CREATE TABLE IF NOT EXISTS embedding_index_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sync_device_state (
            singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
            device_id TEXT NOT NULL,
            display_name TEXT NOT NULL,
            hlc_wall_ms INTEGER NOT NULL,
            hlc_counter INTEGER NOT NULL,
            next_seq INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS sync_mutations (
            platform TEXT NOT NULL,
            platform_session_id TEXT NOT NULL,
            local_seq INTEGER NOT NULL UNIQUE,
            operation TEXT NOT NULL CHECK(operation IN ('upsert', 'delete')),
            version_wall_ms INTEGER NOT NULL,
            version_counter INTEGER NOT NULL,
            version_device_id TEXT NOT NULL,
            content_hash TEXT,
            snapshot_json TEXT,
            PRIMARY KEY(platform, platform_session_id)
        );
        CREATE TABLE IF NOT EXISTS sync_entity_versions (
            platform TEXT NOT NULL,
            platform_session_id TEXT NOT NULL,
            operation TEXT NOT NULL CHECK(operation IN ('upsert', 'delete')),
            version_wall_ms INTEGER NOT NULL,
            version_counter INTEGER NOT NULL,
            version_device_id TEXT NOT NULL,
            content_hash TEXT,
            PRIMARY KEY(platform, platform_session_id)
        );
        CREATE TABLE IF NOT EXISTS sync_remote_cursors (
            generation_id TEXT NOT NULL,
            remote_device_id TEXT NOT NULL,
            cursor_seq INTEGER NOT NULL,
            anchor_end_seq INTEGER,
            anchor_path TEXT,
            anchor_sha256 TEXT,
            updated_at_ms INTEGER NOT NULL,
            PRIMARY KEY(generation_id, remote_device_id)
        );
        CREATE TABLE IF NOT EXISTS sync_published_bundles (
            bundle_sha256 TEXT PRIMARY KEY,
            generation_id TEXT NOT NULL,
            device_id TEXT,
            object_path TEXT,
            start_seq INTEGER,
            end_seq INTEGER,
            bundle_bytes BLOB,
            stage TEXT NOT NULL CHECK(stage IN ('staged', 'published')),
            staged_at_ms INTEGER NOT NULL,
            published_at_ms INTEGER
        );
        CREATE TABLE IF NOT EXISTS sync_publication_state (
            singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
            vault_id TEXT NOT NULL,
            generation_id TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS sync_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            trigger TEXT NOT NULL,
            started_at_ms INTEGER NOT NULL,
            finished_at_ms INTEGER,
            status TEXT NOT NULL,
            error_code TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_sync_mutations_local_seq ON sync_mutations(local_seq);
        CREATE INDEX IF NOT EXISTS idx_sync_runs_started_at ON sync_runs(started_at_ms);
        CREATE INDEX IF NOT EXISTS idx_sync_bundles_stage ON sync_published_bundles(stage);",
    )
    .execute(pool)
    .await?;
    ensure_sync_remote_cursor_columns(pool).await?;
    ensure_sync_published_bundle_columns(pool).await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_sync_bundles_recovery
         ON sync_published_bundles(generation_id, device_id, stage, start_seq);",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS maintenance_progress (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )
    .execute(pool)
    .await?;
    ensure_embedding_vec_table(pool, None).await?;
    Ok(())
}

async fn ensure_embedding_chunks_foreign_key(pool: &SqlitePool) -> Result<()> {
    let sql: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'embedding_chunks'",
    )
    .fetch_optional(pool)
    .await?;

    let Some(sql) = sql else {
        return Ok(());
    };

    if sql.to_ascii_lowercase().contains("references sessions") {
        return Ok(());
    }

    let mut tx = pool.begin().await?;
    sqlx::query(
        "CREATE TABLE embedding_chunks_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            message_id TEXT NOT NULL,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
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
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO embedding_chunks_new (id, message_id, session_id, platform, chunk_index, role, text, content_hash, backend_id, model_id, dim, status, error, updated_at)
         SELECT c.id, c.message_id, c.session_id, c.platform, c.chunk_index, c.role, c.text, c.content_hash, c.backend_id, c.model_id, c.dim, c.status, c.error, c.updated_at
         FROM embedding_chunks c
         WHERE EXISTS (SELECT 1 FROM sessions s WHERE s.id = c.session_id);",
    )
    .execute(&mut *tx)
    .await?;

    let has_embedding_vec: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'embedding_vec')",
    )
    .fetch_one(&mut *tx)
    .await?;

    if has_embedding_vec {
        // Orphan vectors must be purged in the same transaction as the table
        // rebuild: a swallowed failure here would leave embedding_vec rows that
        // point at dropped chunk ids. Propagating the error rolls the whole
        // rebuild back so the legacy schema stays consistent.
        sqlx::query(
            "DELETE FROM embedding_vec WHERE chunk_id NOT IN (SELECT id FROM embedding_chunks_new);",
        )
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query("DROP TABLE embedding_chunks;")
        .execute(&mut *tx)
        .await?;

    sqlx::query("ALTER TABLE embedding_chunks_new RENAME TO embedding_chunks;")
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(())
}

async fn ensure_sync_remote_cursor_columns(pool: &SqlitePool) -> Result<()> {
    let existing: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('sync_remote_cursors')")
            .fetch_all(pool)
            .await?;
    let missing = [
        ("anchor_end_seq", "INTEGER"),
        ("anchor_path", "TEXT"),
        ("anchor_sha256", "TEXT"),
    ]
    .into_iter()
    .filter(|(name, _)| !existing.iter().any(|column| column == name))
    .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }

    let mut tx = pool.begin().await?;
    for (name, definition) in missing {
        sqlx::query(&format!(
            "ALTER TABLE sync_remote_cursors ADD COLUMN {name} {definition}"
        ))
        .execute(&mut *tx)
        .await?;
    }
    // A released database stored only the numeric cursor. Resetting it forces an
    // idempotent LWW replay so the first new cursor is bound to verified bytes.
    sqlx::query(
        "UPDATE sync_remote_cursors
         SET cursor_seq = 0, anchor_end_seq = NULL, anchor_path = NULL, anchor_sha256 = NULL",
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn ensure_sync_published_bundle_columns(pool: &SqlitePool) -> Result<()> {
    let existing: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('sync_published_bundles')")
            .fetch_all(pool)
            .await?;
    for (name, definition) in [
        ("device_id", "TEXT"),
        ("object_path", "TEXT"),
        ("start_seq", "INTEGER"),
        ("end_seq", "INTEGER"),
        ("bundle_bytes", "BLOB"),
    ] {
        if !existing.iter().any(|column| column == name) {
            sqlx::query(&format!(
                "ALTER TABLE sync_published_bundles ADD COLUMN {name} {definition}"
            ))
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

async fn initialize_session_fts(pool: &SqlitePool) -> Result<()> {
    let mut tx = pool.begin().await?;
    let fts_existed: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'session_fts'
        )",
    )
    .fetch_one(&mut *tx)
    .await?;
    let ids_existed: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'session_fts_ids'
        )",
    )
    .fetch_one(&mut *tx)
    .await?;
    let rebuild = !fts_existed || !ids_existed;
    if rebuild && fts_existed {
        sqlx::query("DROP TABLE session_fts")
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS session_fts_ids (
            fts_rowid INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL UNIQUE REFERENCES sessions(id) ON DELETE CASCADE
        );",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "CREATE VIRTUAL TABLE IF NOT EXISTS session_fts USING fts5(
            session_id UNINDEXED,
            title,
            content,
            tokenize = 'trigram'
        );",
    )
    .execute(&mut *tx)
    .await?;
    if rebuild {
        sqlx::query("DELETE FROM session_fts_ids")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT OR IGNORE INTO session_fts_ids(session_id)
             SELECT id FROM sessions;",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO session_fts(rowid, session_id, title, content)
             SELECT ids.fts_rowid, s.id, COALESCE(s.title, ''), COALESCE((
                 SELECT group_concat(ordered.content, char(10))
             FROM (
                 SELECT m.content AS content
                 FROM messages m
                 WHERE m.session_id = s.id
                 ORDER BY m.seq
             ) ordered
             ), '')
             FROM sessions s
             INNER JOIN session_fts_ids ids ON ids.session_id = s.id;",
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Ensure the active sqlite-vec table matches `dimensions`.
/// When dimensions change, drop and recreate the virtual table.
pub async fn ensure_embedding_vec_table(
    pool: &SqlitePool,
    dimensions: Option<usize>,
) -> Result<()> {
    let current: Option<String> =
        sqlx::query_scalar("SELECT value FROM embedding_index_meta WHERE key = 'vec_dimensions'")
            .fetch_optional(pool)
            .await?;
    let table_sql: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'embedding_vec'",
    )
    .fetch_optional(pool)
    .await?;
    let inferred = table_sql.as_deref().and_then(infer_vec_dimensions);
    let current_dim = current
        .and_then(|value| value.parse::<usize>().ok())
        .or(inferred);
    // During schema bootstrap, preserve an existing table. The active backend will
    // pass an explicit dimension immediately after settings are loaded.
    let desired = dimensions.or(current_dim).unwrap_or(512).clamp(8, 4096);
    let table_exists = table_sql.is_some();
    let needs_rebuild = !table_exists || current_dim != Some(desired);
    if !needs_rebuild {
        if current_dim == Some(desired) {
            sqlx::query(
                "INSERT INTO embedding_index_meta(key, value) VALUES('vec_dimensions', ?)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            )
            .bind(desired.to_string())
            .execute(pool)
            .await?;
        }
        return Ok(());
    }

    tracing::info!(
        previous = ?current_dim,
        desired,
        "recreating embedding_vec for active dimensions"
    );
    let mut tx = pool.begin().await?;
    sqlx::query("DROP TABLE IF EXISTS embedding_vec;")
        .execute(&mut *tx)
        .await?;
    sqlx::query(&format!(
        "CREATE VIRTUAL TABLE embedding_vec USING vec0(
            chunk_id INTEGER PRIMARY KEY,
            embedding float[{desired}] distance_metric=cosine,
            +session_id TEXT,
            +message_id TEXT,
            +platform TEXT
        );"
    ))
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO embedding_index_meta(key, value) VALUES('vec_dimensions', ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(desired.to_string())
    .execute(&mut *tx)
    .await?;
    // Active vectors are gone; mark ready chunks pending so they re-embed under new dim.
    sqlx::query(
        "UPDATE embedding_chunks SET status = 'pending', error = NULL, dim = ?, updated_at = ? WHERE status = 'ready'",
    )
    .bind(desired as i64)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn activate_embedding_index(
    pool: &SqlitePool,
    backend_id: &str,
    model_id: &str,
) -> Result<bool> {
    let current_backend: Option<String> =
        sqlx::query_scalar("SELECT value FROM embedding_index_meta WHERE key = 'active_backend'")
            .fetch_optional(pool)
            .await?;
    let current_model: Option<String> =
        sqlx::query_scalar("SELECT value FROM embedding_index_meta WHERE key = 'active_model'")
            .fetch_optional(pool)
            .await?;
    let changed = current_backend.as_deref() != Some(backend_id)
        || current_model.as_deref() != Some(model_id);
    if !changed {
        return Ok(false);
    }

    tracing::info!(
        previous_backend = ?current_backend,
        previous_model = ?current_model,
        backend_id,
        model_id,
        "activating new embedding vector space"
    );
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM embedding_vec")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM embedding_chunks")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO embedding_index_meta(key, value) VALUES('active_backend', ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(backend_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO embedding_index_meta(key, value) VALUES('active_model', ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(model_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

fn infer_vec_dimensions(sql: &str) -> Option<usize> {
    let marker = "float[";
    let start = sql.to_ascii_lowercase().find(marker)? + marker.len();
    let tail = &sql[start..];
    let end = tail.find(']')?;
    tail[..end].trim().parse().ok()
}

/// Row budget for the incremental startup timestamp normalization on large
/// databases: each launch normalizes at most `BATCH_ROWS * MAX_BATCHES` rows
/// per (table, column) and records a rowid cursor in `maintenance_progress`,
/// so mixed-format timestamps converge over several launches without a
/// multi-second full-table scan blocking startup.
const TIMESTAMP_NORMALIZE_BATCH_ROWS: i64 = 1_000;
const TIMESTAMP_NORMALIZE_MAX_BATCHES: usize = 8;

async fn maybe_normalize_stored_timestamps(pool: &SqlitePool) -> Result<()> {
    let message_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    // Large archives normalize incrementally: bounded batches per launch keep
    // startup interactive while the cursor in maintenance_progress advances
    // until every timestamp is rewritten.
    if message_count > 2_000 {
        return normalize_stored_timestamps_batched(
            pool,
            TIMESTAMP_NORMALIZE_BATCH_ROWS,
            TIMESTAMP_NORMALIZE_MAX_BATCHES,
        )
        .await;
    }
    // Small/fresh databases converge in a single pass; also clear any
    // incremental cursor left over from an earlier large state.
    normalize_stored_timestamps(pool).await?;
    clear_timestamp_normalize_progress(pool).await
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

async fn clear_timestamp_normalize_progress(pool: &SqlitePool) -> Result<()> {
    sqlx::query("DELETE FROM maintenance_progress WHERE key LIKE 'timestamp_normalize:%'")
        .execute(pool)
        .await?;
    Ok(())
}

/// Incrementally rewrite mixed-format timestamps to the canonical epoch-seconds
/// text form, advancing at most `batch_rows * max_batches` rows per (table,
/// column) per call. The rowid cursor persists in `maintenance_progress`
/// between calls so repeated launches eventually cover the whole table.
async fn normalize_stored_timestamps_batched(
    pool: &SqlitePool,
    batch_rows: i64,
    max_batches: usize,
) -> Result<()> {
    for (table, column) in [
        ("sessions", "created_at"),
        ("sessions", "updated_at"),
        ("messages", "created_at"),
    ] {
        let key = format!("timestamp_normalize:{table}.{column}");
        let stored_cursor: Option<String> =
            sqlx::query_scalar("SELECT value FROM maintenance_progress WHERE key = ?")
                .bind(&key)
                .fetch_optional(pool)
                .await?;
        let mut cursor: i64 = stored_cursor
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let expression = timestamp::expression(column);
        for _ in 0..max_batches {
            let batch_end: Option<i64> = sqlx::query_scalar(&format!(
                "SELECT MAX(rowid) FROM (
                     SELECT rowid FROM {table} WHERE rowid > ? ORDER BY rowid LIMIT ?
                 )"
            ))
            .bind(cursor)
            .bind(batch_rows)
            .fetch_one(pool)
            .await?;
            let Some(batch_end) = batch_end else {
                // No rows left beyond the cursor: this (table, column) is done.
                sqlx::query("DELETE FROM maintenance_progress WHERE key = ?")
                    .bind(&key)
                    .execute(pool)
                    .await?;
                break;
            };
            // The final comparison skips rows already in canonical form so
            // repeated passes do not rewrite unchanged rows.
            let updated = sqlx::query(&format!(
                "UPDATE {table} SET {column} = CAST(({expression}) AS TEXT)
                 WHERE rowid > ? AND rowid <= ?
                   AND {column} IS NOT NULL
                   AND ({expression}) IS NOT NULL
                   AND {column} <> CAST(({expression}) AS TEXT)"
            ))
            .bind(cursor)
            .bind(batch_end)
            .execute(pool)
            .await?
            .rows_affected();
            if updated > 0 {
                tracing::info!(
                    table,
                    column,
                    rows = updated,
                    from_rowid = cursor,
                    to_rowid = batch_end,
                    "incremental timestamp normalization batch"
                );
            }
            cursor = batch_end;
            sqlx::query(
                "INSERT INTO maintenance_progress(key, value) VALUES (?, ?)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            )
            .bind(&key)
            .bind(cursor.to_string())
            .execute(pool)
            .await?;
        }
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

    #[tokio::test]
    async fn initializes_sync_tables() {
        register_sqlite_vec();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        initialize_schema(&pool).await.unwrap();
        for table in [
            "sync_device_state",
            "sync_mutations",
            "sync_entity_versions",
            "sync_remote_cursors",
            "sync_published_bundles",
            "sync_publication_state",
            "sync_runs",
        ] {
            let exists: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
            )
            .bind(table)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(exists, 1, "missing sync table {table}");
        }
    }

    #[tokio::test]
    async fn upgrades_legacy_published_bundle_table_for_recoverable_staging() {
        register_sqlite_vec();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE sync_published_bundles (
                bundle_sha256 TEXT PRIMARY KEY,
                generation_id TEXT NOT NULL,
                stage TEXT NOT NULL CHECK(stage IN ('staged', 'published')),
                staged_at_ms INTEGER NOT NULL,
                published_at_ms INTEGER
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        initialize_schema(&pool).await.unwrap();

        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('sync_published_bundles')")
                .fetch_all(&pool)
                .await
                .unwrap();
        for expected in [
            "device_id",
            "object_path",
            "start_seq",
            "end_seq",
            "bundle_bytes",
        ] {
            assert!(
                columns.iter().any(|column| column == expected),
                "missing upgraded column {expected}"
            );
        }
    }

    #[tokio::test]
    async fn upgrades_unanchored_remote_cursors_by_forcing_safe_replay() {
        register_sqlite_vec();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE sync_remote_cursors (
                generation_id TEXT NOT NULL,
                remote_device_id TEXT NOT NULL,
                cursor_seq INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY(generation_id, remote_device_id)
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sync_remote_cursors
             (generation_id, remote_device_id, cursor_seq, updated_at_ms)
             VALUES ('generation', 'device-old', 7, 42)",
        )
        .execute(&pool)
        .await
        .unwrap();

        initialize_schema(&pool).await.unwrap();

        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('sync_remote_cursors')")
                .fetch_all(&pool)
                .await
                .unwrap();
        for expected in ["anchor_end_seq", "anchor_path", "anchor_sha256"] {
            assert!(
                columns.iter().any(|column| column == expected),
                "missing upgraded column {expected}"
            );
        }
        let migrated: (i64, Option<i64>, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT cursor_seq, anchor_end_seq, anchor_path, anchor_sha256
             FROM sync_remote_cursors
             WHERE generation_id = 'generation' AND remote_device_id = 'device-old'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(migrated, (0, None, None, None));
    }

    #[tokio::test]
    async fn initializes_and_backfills_session_fts_for_existing_data() {
        register_sqlite_vec();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE sessions (id TEXT PRIMARY KEY, platform TEXT NOT NULL, platform_session_id TEXT NOT NULL, title TEXT, created_at TEXT, updated_at TEXT, imported_at TEXT, raw_data TEXT, UNIQUE(platform, platform_session_id)); CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, role TEXT NOT NULL, content TEXT, metadata TEXT, created_at TEXT, seq INTEGER);")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO sessions (id, platform, platform_session_id, title) VALUES ('s1', 'test', 'source-1', '数据库设计')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO messages (id, session_id, role, content, metadata, seq) VALUES ('m1', 's1', 'user', '使用全文检索提升速度', '{}', 0)")
            .execute(&pool)
            .await
            .unwrap();

        initialize_schema(&pool).await.unwrap();

        let title_id: String = sqlx::query_scalar(
            "SELECT session_id FROM session_fts WHERE session_fts MATCH '库设计'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let content_id: String = sqlx::query_scalar(
            "SELECT session_id FROM session_fts WHERE session_fts MATCH '文检索'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let mapped_rowid: i64 =
            sqlx::query_scalar("SELECT fts_rowid FROM session_fts_ids WHERE session_id = 's1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let fts_rowid: i64 =
            sqlx::query_scalar("SELECT rowid FROM session_fts WHERE session_id = 's1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(title_id, "s1");
        assert_eq!(content_id, "s1");
        assert_eq!(fts_rowid, mapped_rowid);
    }

    #[tokio::test]
    async fn rebuilds_session_fts_when_rowid_mapping_is_missing() {
        register_sqlite_vec();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE sessions (id TEXT PRIMARY KEY, platform TEXT NOT NULL, platform_session_id TEXT NOT NULL, title TEXT, created_at TEXT, updated_at TEXT, imported_at TEXT, raw_data TEXT, UNIQUE(platform, platform_session_id)); CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, role TEXT NOT NULL, content TEXT, metadata TEXT, created_at TEXT, seq INTEGER); CREATE VIRTUAL TABLE session_fts USING fts5(session_id UNINDEXED, title, content, tokenize = 'trigram');")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO sessions (id, platform, platform_session_id, title) VALUES ('s1', 'test', 'source-1', '旧版索引'); INSERT INTO session_fts(rowid, session_id, title, content) VALUES (7, 's1', '旧版索引', '需要重建映射');")
            .execute(&pool)
            .await
            .unwrap();

        initialize_schema(&pool).await.unwrap();

        let mapped_rowid: i64 =
            sqlx::query_scalar("SELECT fts_rowid FROM session_fts_ids WHERE session_id = 's1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let fts_rowid: i64 =
            sqlx::query_scalar("SELECT rowid FROM session_fts WHERE session_id = 's1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let fts_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_fts")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(mapped_rowid, fts_rowid);
        assert_eq!(fts_count, 1);
    }
}

#[cfg(test)]
mod vec_dimension_tests {
    use super::*;

    #[test]
    fn infers_existing_vec_dimensions() {
        assert_eq!(
            infer_vec_dimensions(
                "CREATE VIRTUAL TABLE embedding_vec USING vec0(embedding float[768])"
            ),
            Some(768)
        );
        assert_eq!(infer_vec_dimensions("CREATE TABLE other(x TEXT)"), None);
    }
}

#[cfg(test)]
mod active_index_tests {
    use super::*;

    #[tokio::test]
    async fn switching_active_model_clears_old_mapping() {
        register_sqlite_vec();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        initialize_schema(&pool).await.unwrap();
        assert!(
            activate_embedding_index(&pool, "local", "model-a")
                .await
                .unwrap()
        );
        sqlx::query(
            "INSERT INTO sessions (id, platform, platform_session_id) VALUES ('s', 'test', 'platform-s')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO embedding_chunks
             (message_id, session_id, platform, chunk_index, role, text, content_hash,
              backend_id, model_id, dim, status, updated_at)
             VALUES ('m', 's', 'test', 0, 'user', 'text', 'hash',
                     'local', 'model-a', 512, 'pending', 'now')",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            !activate_embedding_index(&pool, "local", "model-a")
                .await
                .unwrap()
        );
        assert!(
            activate_embedding_index(&pool, "local", "model-b")
                .await
                .unwrap()
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM embedding_chunks")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn upgrades_legacy_embedding_chunks_table_and_purges_orphans() {
        register_sqlite_vec();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                platform TEXT NOT NULL,
                platform_session_id TEXT NOT NULL,
                title TEXT,
                created_at TEXT,
                updated_at TEXT,
                imported_at TEXT DEFAULT CURRENT_TIMESTAMP,
                raw_data TEXT,
                UNIQUE(platform, platform_session_id)
            );
            CREATE TABLE embedding_chunks (
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
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO sessions (id, platform, platform_session_id) VALUES ('s1', 'test', 'p1')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Valid chunk (session exists)
        sqlx::query(
            "INSERT INTO embedding_chunks
             (id, message_id, session_id, platform, chunk_index, role, text, content_hash,
              backend_id, model_id, dim, status, updated_at)
             VALUES (1, 'm1', 's1', 'test', 0, 'user', 'valid', 'hash1',
                     'local', 'model-a', 512, 'ready', 'now')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Orphan chunk (session 's2' does not exist)
        sqlx::query(
            "INSERT INTO embedding_chunks
             (id, message_id, session_id, platform, chunk_index, role, text, content_hash,
              backend_id, model_id, dim, status, updated_at)
             VALUES (2, 'm2', 's2', 'test', 0, 'user', 'orphan', 'hash2',
                     'local', 'model-a', 512, 'ready', 'now')",
        )
        .execute(&pool)
        .await
        .unwrap();

        initialize_schema(&pool).await.unwrap();

        let table_sql: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'embedding_chunks'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            table_sql
                .to_ascii_lowercase()
                .contains("references sessions"),
            "table sql should contain foreign key constraint: {table_sql}"
        );

        let chunks: Vec<(i64, String)> =
            sqlx::query_as("SELECT id, session_id FROM embedding_chunks ORDER BY id ASC")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], (1, "s1".to_string()));
    }

    #[tokio::test]
    async fn rebuild_purges_orphan_vectors_during_foreign_key_upgrade() {
        register_sqlite_vec();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                platform TEXT NOT NULL,
                platform_session_id TEXT NOT NULL,
                title TEXT,
                created_at TEXT,
                updated_at TEXT,
                imported_at TEXT DEFAULT CURRENT_TIMESTAMP,
                raw_data TEXT,
                UNIQUE(platform, platform_session_id)
            );
            CREATE TABLE embedding_chunks (
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
            );
            CREATE VIRTUAL TABLE embedding_vec USING vec0(
                chunk_id INTEGER PRIMARY KEY,
                embedding float[8] distance_metric=cosine
            );",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sessions (id, platform, platform_session_id) VALUES ('s1', 'test', 'p1')",
        )
        .execute(&pool)
        .await
        .unwrap();
        for (chunk_id, session_id) in [(1_i64, "s1"), (2_i64, "s2")] {
            sqlx::query(
                "INSERT INTO embedding_chunks
                 (id, message_id, session_id, platform, chunk_index, role, text, content_hash,
                  backend_id, model_id, dim, status, updated_at)
                 VALUES (?, ?, ?, 'test', 0, 'user', 'text', 'hash',
                         'local', 'model-a', 8, 'ready', 'now')",
            )
            .bind(chunk_id)
            .bind(format!("m{chunk_id}"))
            .bind(session_id)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("INSERT INTO embedding_vec(chunk_id, embedding) VALUES (?, ?)")
                .bind(chunk_id)
                .bind(vec![0u8; 32])
                .execute(&pool)
                .await
                .unwrap();
        }

        initialize_schema(&pool).await.unwrap();

        let vector_chunk_ids: Vec<i64> =
            sqlx::query_scalar("SELECT chunk_id FROM embedding_vec ORDER BY chunk_id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            vector_chunk_ids,
            vec![1],
            "orphan vector for dropped chunk must be purged in the rebuild transaction"
        );
    }

    #[tokio::test]
    async fn rebuild_rolls_back_when_orphan_vector_delete_fails() {
        register_sqlite_vec();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                platform TEXT NOT NULL,
                platform_session_id TEXT NOT NULL,
                title TEXT,
                created_at TEXT,
                updated_at TEXT,
                imported_at TEXT DEFAULT CURRENT_TIMESTAMP,
                raw_data TEXT,
                UNIQUE(platform, platform_session_id)
            );
            CREATE TABLE embedding_chunks (
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
            );
            CREATE TABLE embedding_vec (chunk_id INTEGER PRIMARY KEY);
            CREATE TRIGGER embedding_vec_block_delete
            BEFORE DELETE ON embedding_vec
            BEGIN
                SELECT RAISE(ABORT, 'forced orphan vector delete failure');
            END;",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sessions (id, platform, platform_session_id) VALUES ('s1', 'test', 'p1')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO embedding_chunks
             (id, message_id, session_id, platform, chunk_index, role, text, content_hash,
              backend_id, model_id, dim, status, updated_at)
             VALUES (1, 'm1', 's1', 'test', 0, 'user', 'text', 'hash',
                     'local', 'model-a', 4, 'ready', 'now')",
        )
        .execute(&pool)
        .await
        .unwrap();
        // A vector row pointing at a chunk id that the rebuild will drop: the
        // purge DELETE must touch this row and fail via the blocking trigger.
        sqlx::query("INSERT INTO embedding_vec(chunk_id) VALUES (99)")
            .execute(&pool)
            .await
            .unwrap();

        let result = initialize_schema(&pool).await;

        assert!(
            result.is_err(),
            "orphan vector delete failure must propagate instead of being swallowed"
        );
        // The rebuild transaction rolled back: the legacy table (without the
        // foreign key) is still in place instead of a half-upgraded schema.
        let table_sql: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'embedding_chunks'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            !table_sql
                .to_ascii_lowercase()
                .contains("references sessions"),
            "rebuild must roll back so the legacy table stays consistent: {table_sql}"
        );
        let new_table: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'embedding_chunks_new'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(new_table, 0, "staging table must be rolled back");
    }
}

#[cfg(test)]
mod timestamp_normalize_tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    /// Canonical epoch-seconds text produced by timestamp::expression for the
    /// fixtures below (all legacy formats must converge to these values).
    const CANONICAL_CREATED_AT: &str = "1780853706.105";
    const CANONICAL_UPDATED_AT: &str = "1780857306.105";

    async fn normalize_pool() -> SqlitePool {
        register_sqlite_vec();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        initialize_schema(&pool).await.unwrap();
        pool
    }

    async fn insert_legacy_fixture(pool: &SqlitePool, sessions: usize, messages: usize) {
        for index in 0..sessions {
            sqlx::query("INSERT INTO sessions (id, platform, platform_session_id, created_at, updated_at) VALUES (?, 'test', ?, '2026-06-08T01:35:06.105+08:00', '2026-06-08T02:35:06.105+08:00')")
                .bind(format!("s{index}"))
                .bind(format!("p{index}"))
                .execute(pool)
                .await
                .unwrap();
        }
        for index in 0..messages {
            // Alternate between ISO strings, millisecond epochs and second
            // epochs so normalization has to handle every legacy format.
            let created_at = match index % 3 {
                0 => "2026-06-08T01:35:06.105+08:00".to_string(),
                1 => "1780853706105".to_string(),
                _ => "1780853706.105".to_string(),
            };
            sqlx::query("INSERT INTO messages (id, session_id, role, content, created_at, seq) VALUES (?, 's0', 'user', 'x', ?, ?)")
                .bind(format!("m{index}"))
                .bind(created_at)
                .bind(index as i64)
                .execute(pool)
                .await
                .unwrap();
        }
    }

    /// Rows that still deviate from the canonical normalized value.
    async fn non_canonical_count(
        pool: &SqlitePool,
        table: &str,
        column: &str,
        canonical: &str,
    ) -> i64 {
        sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM {table} WHERE {column} IS NULL OR {column} <> ?"
        ))
        .bind(canonical)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn progress_rows(pool: &SqlitePool) -> Vec<(String, String)> {
        sqlx::query_as("SELECT key, value FROM maintenance_progress ORDER BY key")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn large_database_timestamps_normalize_fully_in_bounded_batches() {
        let pool = normalize_pool().await;
        insert_legacy_fixture(&pool, 2, 2_500).await;

        // The production path must no longer permanently skip large databases.
        maybe_normalize_stored_timestamps(&pool).await.unwrap();

        assert_eq!(
            non_canonical_count(&pool, "messages", "created_at", CANONICAL_CREATED_AT).await,
            0
        );
        assert_eq!(
            non_canonical_count(&pool, "sessions", "created_at", CANONICAL_CREATED_AT).await,
            0
        );
        assert_eq!(
            non_canonical_count(&pool, "sessions", "updated_at", CANONICAL_UPDATED_AT).await,
            0
        );
        // A completed pass leaves no cursor behind.
        assert!(progress_rows(&pool).await.is_empty());
    }

    #[tokio::test]
    async fn incremental_timestamp_normalization_advances_in_batches_across_calls() {
        let pool = normalize_pool().await;
        insert_legacy_fixture(&pool, 10, 10).await;

        // One bounded call per target (3 rows of 10): work must stop at the
        // batch budget instead of rewriting the whole table.
        normalize_stored_timestamps_batched(&pool, 3, 1)
            .await
            .unwrap();

        assert_eq!(
            non_canonical_count(&pool, "sessions", "created_at", CANONICAL_CREATED_AT).await,
            7
        );
        assert_eq!(
            non_canonical_count(&pool, "sessions", "updated_at", CANONICAL_UPDATED_AT).await,
            7
        );
        // Messages mix in two already-canonical rows, so one 3-row batch only
        // leaves 5 deviating rows behind.
        assert_eq!(
            non_canonical_count(&pool, "messages", "created_at", CANONICAL_CREATED_AT).await,
            5
        );
        let cursors = progress_rows(&pool).await;
        assert_eq!(cursors.len(), 3, "each target records its own cursor");
        for (key, value) in &cursors {
            assert_eq!(value, "3", "cursor for {key} must persist between calls");
        }

        // Repeated calls converge without ever exceeding the batch budget.
        for _ in 0..10 {
            normalize_stored_timestamps_batched(&pool, 3, 1)
                .await
                .unwrap();
            if non_canonical_count(&pool, "sessions", "created_at", CANONICAL_CREATED_AT).await == 0
                && non_canonical_count(&pool, "sessions", "updated_at", CANONICAL_UPDATED_AT).await
                    == 0
                && non_canonical_count(&pool, "messages", "created_at", CANONICAL_CREATED_AT).await
                    == 0
                && progress_rows(&pool).await.is_empty()
            {
                break;
            }
        }
        assert_eq!(
            non_canonical_count(&pool, "sessions", "created_at", CANONICAL_CREATED_AT).await,
            0
        );
        assert_eq!(
            non_canonical_count(&pool, "sessions", "updated_at", CANONICAL_UPDATED_AT).await,
            0
        );
        assert_eq!(
            non_canonical_count(&pool, "messages", "created_at", CANONICAL_CREATED_AT).await,
            0
        );
        // Converged passes clear the cursor markers.
        assert!(progress_rows(&pool).await.is_empty());

        // An extra call after convergence is a no-op and keeps data intact.
        normalize_stored_timestamps_batched(&pool, 3, 1)
            .await
            .unwrap();
        assert_eq!(
            non_canonical_count(&pool, "messages", "created_at", CANONICAL_CREATED_AT).await,
            0
        );
        let message_total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(message_total, 10);
    }

    #[tokio::test]
    async fn small_database_full_normalization_clears_incremental_cursors() {
        let pool = normalize_pool().await;
        insert_legacy_fixture(&pool, 1, 3).await;
        // Seed a stale cursor as if the database had previously been large.
        sqlx::query(
            "INSERT INTO maintenance_progress (key, value) VALUES ('timestamp_normalize:messages.created_at', '42')",
        )
        .execute(&pool)
        .await
        .unwrap();

        maybe_normalize_stored_timestamps(&pool).await.unwrap();

        assert_eq!(
            non_canonical_count(&pool, "messages", "created_at", CANONICAL_CREATED_AT).await,
            0
        );
        assert_eq!(
            non_canonical_count(&pool, "sessions", "updated_at", CANONICAL_UPDATED_AT).await,
            0
        );
        assert!(progress_rows(&pool).await.is_empty());
    }
}
