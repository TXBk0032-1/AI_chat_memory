pub mod connection;
mod details;
mod imports;
mod maintenance;
pub mod sessions;
pub mod timestamp;

pub use connection::{connect, copy_database};
pub use details::{get_session_branches, get_session_messages, open_session, search_session_hits};
pub use imports::import_sessions;
pub use maintenance::{delete_embedding_vectors_in, delete_session, sync_status};
pub use sessions::{search, search_and_count};

#[cfg(test)]
use crate::models::{SearchHitField, SearchQuery};
#[cfg(test)]
use connection::normalize_stored_timestamps;
#[cfg(test)]
use sqlx::sqlite::SqlitePoolOptions;

#[cfg(test)]
use sqlx::SqlitePool;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        models::NormalizedSession,
        normalizer::normalize_session,
        sync::{
            store::SyncStore,
            types::{EntityKey, MutationOperation, NormalizedSessionSnapshot, SyncMessageSnapshot},
        },
    };
    use serde_json::json;

    async fn create_detail_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE sessions (id TEXT PRIMARY KEY, platform TEXT NOT NULL, platform_session_id TEXT NOT NULL, title TEXT, created_at TEXT, updated_at TEXT, imported_at TEXT, raw_data TEXT, UNIQUE(platform, platform_session_id)); CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, role TEXT NOT NULL, content TEXT, metadata TEXT, created_at TEXT, seq INTEGER);")
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    async fn create_sync_pool() -> SqlitePool {
        connection::register_sqlite_vec();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        connection::initialize_schema(&pool).await.unwrap();
        pool
    }

    fn session_fixture(remote_id: &str, title: &str) -> NormalizedSession {
        normalize_session(
            "custom",
            &json!({
                "id": remote_id,
                "title": title,
                "created_at": "2026-01-02T03:04:05Z",
                "updated_at": "2026-01-02T04:05:06Z",
                "messages": [
                    {
                        "role": "user",
                        "content": format!("question-{remote_id}"),
                        "metadata": {"turn": 1},
                        "created_at": "2026-01-02T03:05:00Z"
                    },
                    {
                        "role": "assistant",
                        "content": format!("answer-{remote_id}"),
                        "metadata": {"turn": 2},
                        "created_at": "2026-01-02T03:06:00Z"
                    }
                ]
            }),
        )
        .unwrap()
    }

    async fn table_count(pool: &SqlitePool, table: &str) -> i64 {
        sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn missing_session_messages_returns_not_found() {
        let pool = create_detail_pool().await;

        let result = get_session_messages(&pool, "missing", 0, 50).await;

        assert!(matches!(
            result,
            Err(crate::error::AppError::NotFound(resource)) if resource == "session"
        ));
    }

    #[tokio::test]
    async fn empty_session_messages_returns_empty_list() {
        let pool = create_detail_pool().await;
        sqlx::query("INSERT INTO sessions (id, platform, platform_session_id) VALUES ('empty', 'deepseek', 'empty')")
            .execute(&pool)
            .await
            .unwrap();

        let messages = get_session_messages(&pool, "empty", 0, 50).await.unwrap();

        assert!(messages.is_empty());
    }

    #[tokio::test]
    async fn opens_bounded_window_around_anchor_without_raw_data() {
        let pool = create_detail_pool().await;
        let raw = json!({
            "payload": "x".repeat(1024 * 1024),
            "sources": [{"cite_index": 35, "url": "https://example.com", "title": "Example", "snippet": "Summary"}]
        });
        sqlx::query("INSERT INTO sessions (id, platform, platform_session_id, title, raw_data) VALUES ('long', 'deepseek', 'long', 'Long', ?)")
            .bind(raw.to_string())
            .execute(&pool)
            .await
            .unwrap();
        for seq in 0..1000_i64 {
            sqlx::query("INSERT INTO messages (id, session_id, role, content, metadata, seq) VALUES (?, 'long', 'assistant', ?, '{}', ?)")
                .bind(format!("message-{seq}"))
                .bind(format!("content {seq}"))
                .bind(seq)
                .execute(&pool)
                .await
                .unwrap();
        }

        let opened = open_session(&pool, "long", Some(500)).await.unwrap();
        assert_eq!(opened.message_count, 1000);
        assert_eq!(opened.start_seq, 475);
        assert_eq!(opened.messages.len(), 50);
        assert_eq!(opened.messages[0].seq, 475);
        assert_eq!(opened.messages[49].seq, 524);
        assert_eq!(opened.references.len(), 1);
        assert_eq!(opened.references[0].cite_index, 35);
        assert_eq!(opened.references[0].summary, "Summary");

        let bounded = get_session_messages(&pool, "long", 0, 1000).await.unwrap();
        assert_eq!(bounded.len(), 100);
    }

    #[tokio::test]
    async fn returns_full_text_hits_and_lightweight_branches() {
        let pool = create_detail_pool().await;
        sqlx::query("INSERT INTO sessions (id, platform, platform_session_id, title, raw_data) VALUES ('branch', 'deepseek', 'branch', 'Branch', '{}')")
            .execute(&pool)
            .await
            .unwrap();
        let metadata = json!({
            "source": "deepseek_export",
            "node_id": "node-1",
            "parent_node_id": "root",
            "children_node_ids": ["node-2"],
            "thinking": "Needle in thinking, needle again"
        });
        sqlx::query("INSERT INTO messages (id, session_id, role, content, metadata, seq) VALUES ('message-1', 'branch', 'assistant', ?, ?, 12)")
            .bind(format!("Needle in content {}", "x".repeat(300)))
            .bind(metadata.to_string())
            .execute(&pool)
            .await
            .unwrap();

        let hits = search_session_hits(&pool, "branch", "needle")
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].field, SearchHitField::Content);
        assert_eq!(hits[0].count, 1);
        assert_eq!(hits[1].field, SearchHitField::Thinking);
        assert_eq!(hits[1].count, 2);

        let branches = get_session_branches(&pool, "branch").await.unwrap();
        assert_eq!(branches.nodes.len(), 1);
        assert_eq!(branches.nodes[0].node_id, "node-1");
        assert!(branches.nodes[0].children_node_ids.is_empty());
        assert_eq!(branches.nodes[0].preview.chars().count(), 161);
        assert!(branches.nodes[0].preview.ends_with('…'));
        assert_eq!(branches.default_leaf_node_id, "node-1");
    }

    #[tokio::test]
    async fn reimport_preserves_identity_and_replaces_messages() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("PRAGMA foreign_keys=ON")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE sessions (id TEXT PRIMARY KEY, platform TEXT NOT NULL, platform_session_id TEXT NOT NULL, title TEXT, created_at TEXT, updated_at TEXT, imported_at TEXT, raw_data TEXT, UNIQUE(platform, platform_session_id)); CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, role TEXT NOT NULL, content TEXT, metadata TEXT, created_at TEXT, seq INTEGER); CREATE TABLE session_fts_ids (fts_rowid INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL UNIQUE REFERENCES sessions(id) ON DELETE CASCADE); CREATE VIRTUAL TABLE session_fts USING fts5(session_id UNINDEXED, title, content, tokenize = 'trigram');").execute(&pool).await.unwrap();
        let first = normalize_session(
            "custom",
            &json!({"id":"a","title":"one","messages":[{"role":"user","content":"old"}]}),
        )
        .unwrap();
        import_sessions(&pool, &[first], false).await.unwrap();
        let original: String = sqlx::query_scalar("SELECT id FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        let second = normalize_session(
            "custom",
            &json!({"id":"a","title":"two","messages":[{"role":"assistant","content":"new"}]}),
        )
        .unwrap();
        import_sessions(&pool, &[second], false).await.unwrap();
        let current: String = sqlx::query_scalar("SELECT id FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        let content: String = sqlx::query_scalar("SELECT content FROM messages")
            .fetch_one(&pool)
            .await
            .unwrap();
        let old_hits: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM session_fts WHERE session_fts MATCH 'old'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let new_hits: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM session_fts WHERE session_fts MATCH 'new'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(original, current);
        assert_eq!(content, "new");
        assert_eq!(old_hits, 0);
        assert_eq!(new_hits, 1);
    }

    #[tokio::test]
    async fn search_is_bound_and_delete_cascades() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("PRAGMA foreign_keys=ON")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE sessions (id TEXT PRIMARY KEY, platform TEXT NOT NULL, platform_session_id TEXT NOT NULL, title TEXT, created_at TEXT, updated_at TEXT, imported_at TEXT, raw_data TEXT, UNIQUE(platform, platform_session_id)); CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, role TEXT NOT NULL, content TEXT, metadata TEXT, created_at TEXT, seq INTEGER); CREATE TABLE session_fts_ids (fts_rowid INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL UNIQUE REFERENCES sessions(id) ON DELETE CASCADE); CREATE VIRTUAL TABLE session_fts USING fts5(session_id UNINDEXED, title, content, tokenize = 'trigram');").execute(&pool).await.unwrap();
        let session = normalize_session(
            "custom",
            &json!({"id":"safe","title":"safe","messages":[{"role":"user","content":"hello"}]}),
        )
        .unwrap();
        import_sessions(&pool, &[session], false).await.unwrap();
        let result = search(
            &pool,
            &SearchQuery {
                q: Some("%' OR 1=1 --".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(result.is_empty());
        let id: String = sqlx::query_scalar("SELECT id FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        let indexed_before_delete: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM session_fts WHERE session_id = ?")
                .bind(&id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(indexed_before_delete, 1);
        delete_session(&pool, &id, false).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
        let indexed_after_delete: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM session_fts WHERE session_id = ?")
                .bind(&id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(indexed_after_delete, 0);
    }

    #[tokio::test]
    async fn import_commits_business_data_and_complete_upsert_snapshot() {
        let pool = create_sync_pool().await;
        let store = SyncStore::new(pool.clone());
        store.initialize_device("device-a", "Laptop").await.unwrap();
        let session = session_fixture("remote-a", "Atomic import");

        import_sessions(&pool, std::slice::from_ref(&session), true)
            .await
            .unwrap();

        assert_eq!(table_count(&pool, "sessions").await, 1);
        assert_eq!(table_count(&pool, "messages").await, 2);
        assert_eq!(table_count(&pool, "session_fts").await, 1);
        assert_eq!(table_count(&pool, "sync_mutations").await, 1);
        assert_eq!(table_count(&pool, "sync_entity_versions").await, 1);
        let pending = store.pending_mutations(10).await.unwrap();
        assert_eq!(pending[0].operation, MutationOperation::Upsert);
        assert_eq!(
            pending[0].snapshot,
            Some(NormalizedSessionSnapshot {
                key: EntityKey {
                    platform: session.platform.clone(),
                    platform_session_id: session.platform_session_id.clone(),
                },
                title: session.title.clone(),
                created_at: session.created_at.clone(),
                updated_at: session.updated_at.clone(),
                imported_at: session.imported_at.clone(),
                raw_data: session.raw_data.clone(),
                messages: session
                    .messages
                    .iter()
                    .map(|message| SyncMessageSnapshot {
                        role: message.role.clone(),
                        content: message.content.clone(),
                        metadata: message.metadata.clone(),
                        created_at: message.created_at.clone(),
                    })
                    .collect(),
            })
        );
    }

    #[tokio::test]
    async fn delete_commits_business_delete_and_tombstone() {
        let pool = create_sync_pool().await;
        let session = session_fixture("remote-delete", "Delete me");
        import_sessions(&pool, &[session], false).await.unwrap();
        let id: String = sqlx::query_scalar("SELECT id FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        let store = SyncStore::new(pool.clone());
        store.initialize_device("device-a", "Laptop").await.unwrap();

        delete_session(&pool, &id, true).await.unwrap();

        assert_eq!(table_count(&pool, "sessions").await, 0);
        assert_eq!(table_count(&pool, "messages").await, 0);
        assert_eq!(table_count(&pool, "session_fts").await, 0);
        assert_eq!(table_count(&pool, "sync_mutations").await, 1);
        assert_eq!(table_count(&pool, "sync_entity_versions").await, 1);
        let pending = store.pending_mutations(10).await.unwrap();
        assert_eq!(pending[0].operation, MutationOperation::Delete);
        assert_eq!(pending[0].key.platform, "custom");
        assert_eq!(pending[0].key.platform_session_id, "remote-delete");
        assert!(pending[0].snapshot.is_none());
    }

    #[tokio::test]
    async fn record_sync_false_skips_import_and_delete_outbox() {
        let pool = create_sync_pool().await;
        let store = SyncStore::new(pool.clone());
        store.initialize_device("device-a", "Laptop").await.unwrap();

        import_sessions(&pool, &[session_fixture("remote-off", "Off")], false)
            .await
            .unwrap();
        let id: String = sqlx::query_scalar("SELECT id FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        delete_session(&pool, &id, false).await.unwrap();

        assert_eq!(table_count(&pool, "sync_mutations").await, 0);
        assert_eq!(table_count(&pool, "sync_entity_versions").await, 0);
        assert_eq!(store.device_state().await.unwrap().unwrap().next_seq, 1);
    }

    #[tokio::test]
    async fn uninitialized_device_skips_import_and_delete_outbox() {
        let pool = create_sync_pool().await;

        import_sessions(
            &pool,
            &[session_fixture("remote-uninitialized", "No device")],
            true,
        )
        .await
        .unwrap();
        let id: String = sqlx::query_scalar("SELECT id FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        delete_session(&pool, &id, true).await.unwrap();

        assert_eq!(table_count(&pool, "sessions").await, 0);
        assert_eq!(table_count(&pool, "sync_mutations").await, 0);
        assert_eq!(table_count(&pool, "sync_entity_versions").await, 0);
    }

    #[tokio::test]
    async fn import_rolls_back_failed_session_but_keeps_prior_committed_sessions() {
        let pool = create_sync_pool().await;
        let store = SyncStore::new(pool.clone());
        store.initialize_device("device-a", "Laptop").await.unwrap();
        sqlx::query("UPDATE sync_device_state SET hlc_wall_ms = ?, hlc_counter = ?, next_seq = 7")
            .bind(i64::MAX)
            .bind(i64::MAX - 1)
            .execute(&pool)
            .await
            .unwrap();

        // import_sessions commits each session in its own transaction so a
        // large import does not hold a single write lock across every session.
        // The first session commits before the HLC counter saturates; the second
        // session's sync write overflows the HLC and fails, rolling back only that
        // session and surfacing the error with the count already imported.
        let error = import_sessions(
            &pool,
            &[
                session_fixture("remote-first", "First"),
                session_fixture("remote-second", "Second"),
            ],
            true,
        )
        .await
        .unwrap_err();

        assert!(matches!(error, crate::error::AppError::InvalidData(_)));
        // import_sessions commits each session in its own transaction.
        // The first session fully commits (business rows + one sync mutation) before
        // the HLC saturates; the second session's sync write overflows the HLC, fails,
        // and rolls back only that session. The count already imported is surfaced
        // via the returned error. The first session's 2 messages therefore persist,
        // while the second session never leaves any rows behind.
        assert_eq!(table_count(&pool, "sessions").await, 1);
        assert_eq!(table_count(&pool, "messages").await, 2);
        assert_eq!(table_count(&pool, "session_fts").await, 1);
        // The first session queued exactly one sync mutation before the overflow.
        assert_eq!(table_count(&pool, "sync_mutations").await, 1);
        assert_eq!(table_count(&pool, "sync_entity_versions").await, 1);
        let state = store.device_state().await.unwrap().unwrap();
        // The first session advanced the clock and the sequence before the second
        // failed; the overflow did not corrupt the persisted state.
        assert_eq!(state.hlc_wall_ms, i64::MAX);
        assert_eq!(state.hlc_counter, i64::MAX);
        assert_eq!(state.next_seq, 8);
    }

    #[tokio::test]
    async fn delete_rolls_back_business_data_when_sync_write_fails() {
        let pool = create_sync_pool().await;
        import_sessions(
            &pool,
            &[session_fixture("remote-delete-rollback", "Keep me")],
            false,
        )
        .await
        .unwrap();
        let id: String = sqlx::query_scalar("SELECT id FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        let store = SyncStore::new(pool.clone());
        store.initialize_device("device-a", "Laptop").await.unwrap();
        sqlx::query("UPDATE sync_device_state SET hlc_wall_ms = ?, hlc_counter = ?, next_seq = 11")
            .bind(i64::MAX)
            .bind(i64::MAX)
            .execute(&pool)
            .await
            .unwrap();

        let error = delete_session(&pool, &id, true).await.unwrap_err();

        assert!(matches!(error, crate::error::AppError::InvalidData(_)));
        assert_eq!(table_count(&pool, "sessions").await, 1);
        assert_eq!(table_count(&pool, "messages").await, 2);
        assert_eq!(table_count(&pool, "session_fts").await, 1);
        assert_eq!(table_count(&pool, "sync_mutations").await, 0);
        assert_eq!(table_count(&pool, "sync_entity_versions").await, 0);
        assert_eq!(store.device_state().await.unwrap().unwrap().next_seq, 11);
    }

    #[tokio::test]
    async fn normalizes_mixed_timestamps_and_filters_by_epoch_range() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE sessions (id TEXT PRIMARY KEY, platform TEXT NOT NULL, platform_session_id TEXT NOT NULL, title TEXT, created_at TEXT, updated_at TEXT, imported_at TEXT, raw_data TEXT, UNIQUE(platform, platform_session_id)); CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, role TEXT NOT NULL, content TEXT, metadata TEXT, created_at TEXT, seq INTEGER);")
            .execute(&pool)
            .await
            .unwrap();
        for (id, updated_at) in [
            ("iso", "2026-06-08T01:35:06.105+08:00"),
            ("seconds", "1780853706.105"),
            ("milliseconds", "1780853706105"),
            ("older", "1749317706.105"),
        ] {
            sqlx::query("INSERT INTO sessions (id, platform, platform_session_id, title, updated_at) VALUES (?, 'deepseek', ?, ?, ?)")
                .bind(id)
                .bind(id)
                .bind(id)
                .bind(updated_at)
                .execute(&pool)
                .await
                .unwrap();
        }
        normalize_stored_timestamps(&pool).await.unwrap();
        let rows = search(
            &pool,
            &SearchQuery {
                date_from: Some("1780853706".into()),
                date_to: Some("1780853707".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.id != "older"));
        let distinct: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT updated_at) FROM sessions WHERE id != 'older'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(distinct, 1);
    }

    #[tokio::test]
    async fn delete_session_cleans_embedding_chunks_and_vector_table() {
        let pool = create_sync_pool().await;
        let session = session_fixture("session-vec", "Vector session");
        import_sessions(&pool, std::slice::from_ref(&session), false)
            .await
            .unwrap();
        let session_id: String =
            sqlx::query_scalar("SELECT id FROM sessions WHERE platform_session_id = 'session-vec'")
                .fetch_one(&pool)
                .await
                .unwrap();

        sqlx::query(
            "INSERT INTO embedding_chunks
             (id, message_id, session_id, platform, chunk_index, role, text, content_hash,
              backend_id, model_id, dim, status, updated_at)
             VALUES (101, 'm1', ?, 'custom', 0, 'user', 'hello', 'h1', 'local', 'model-a', 512, 'ready', 'now')",
        )
        .bind(&session_id)
        .execute(&pool)
        .await
        .unwrap();

        let zero_vec = vec![0u8; 512 * 4];
        sqlx::query(
            "INSERT INTO embedding_vec (chunk_id, embedding, session_id, message_id, platform) VALUES (101, ?, ?, 'm1', 'custom')",
        )
        .bind(zero_vec)
        .bind(&session_id)
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(table_count(&pool, "embedding_chunks").await, 1);
        assert_eq!(table_count(&pool, "embedding_vec").await, 1);

        delete_session(&pool, &session_id, false).await.unwrap();

        assert_eq!(table_count(&pool, "sessions").await, 0);
        assert_eq!(table_count(&pool, "messages").await, 0);
        assert_eq!(table_count(&pool, "embedding_chunks").await, 0);
        assert_eq!(table_count(&pool, "embedding_vec").await, 0);
    }

    #[tokio::test]
    async fn search_session_hits_counts_mixed_case_occurrences_and_skips_missing_fields() {
        let pool = create_detail_pool().await;
        sqlx::query("INSERT INTO sessions (id, platform, platform_session_id) VALUES ('case', 'deepseek', 'case')")
            .execute(&pool)
            .await
            .unwrap();
        let thinking_metadata = json!({"thinking": "a NEEDLE here"});
        sqlx::query("INSERT INTO messages (id, session_id, role, content, metadata, seq) VALUES ('with-thinking', 'case', 'assistant', 'Needle NEEDLE needle', ?, 1)")
            .bind(thinking_metadata.to_string())
            .execute(&pool)
            .await
            .unwrap();
        // NULL thinking and NULL content must not produce hits or errors.
        sqlx::query("INSERT INTO messages (id, session_id, role, content, metadata, seq) VALUES ('bare', 'case', 'user', 'nothing relevant', NULL, 2)")
            .execute(&pool)
            .await
            .unwrap();

        let hits = search_session_hits(&pool, "case", "NeEdLe").await.unwrap();

        assert_eq!(hits.len(), 2);
        let content = hits
            .iter()
            .find(|hit| hit.field == SearchHitField::Content)
            .unwrap();
        assert_eq!(content.message_id, "with-thinking");
        assert_eq!(content.count, 3, "content lowercased once and counted");
        let thinking = hits
            .iter()
            .find(|hit| hit.field == SearchHitField::Thinking)
            .unwrap();
        assert_eq!(thinking.message_id, "with-thinking");
        assert_eq!(thinking.count, 1);
    }

    #[tokio::test]
    async fn search_session_hits_folds_non_ascii_case_like_rust_not_sqlite() {
        let pool = create_detail_pool().await;
        sqlx::query("INSERT INTO sessions (id, platform, platform_session_id) VALUES ('unicode', 'deepseek', 'unicode')")
            .execute(&pool)
            .await
            .unwrap();
        let thinking_metadata = json!({"thinking": "Жёлтый ЯБЛОКО рядом"});
        sqlx::query("INSERT INTO messages (id, session_id, role, content, metadata, seq) VALUES ('umlaut', 'unicode', 'assistant', 'Äpfel und ÉLÉPHANT und ÄPFEL', ?, 1)")
            .bind(thinking_metadata.to_string())
            .execute(&pool)
            .await
            .unwrap();

        // SQLite's lower() leaves non-ASCII uppercase untouched, so an SQL
        // prefilter would drop this row entirely. Folding must match Rust
        // semantics in both directions.
        let content_hits = search_session_hits(&pool, "unicode", "äpfel")
            .await
            .unwrap();
        assert_eq!(content_hits.len(), 1);
        assert_eq!(content_hits[0].field, SearchHitField::Content);
        assert_eq!(content_hits[0].message_id, "umlaut");
        assert_eq!(
            content_hits[0].count, 2,
            "Äpfel and ÄPFEL both fold to äpfel"
        );

        let accent_hits = search_session_hits(&pool, "unicode", "ÉLÉPHANT")
            .await
            .unwrap();
        assert_eq!(accent_hits.len(), 1);
        assert_eq!(accent_hits[0].count, 1);

        let thinking_hits = search_session_hits(&pool, "unicode", "яблоко")
            .await
            .unwrap();
        assert_eq!(thinking_hits.len(), 1);
        assert_eq!(thinking_hits[0].field, SearchHitField::Thinking);
        assert_eq!(thinking_hits[0].count, 1);
    }

    #[tokio::test]
    async fn sync_status_returns_normalized_epoch_string() {
        let pool = create_detail_pool().await;
        sqlx::query("INSERT INTO sessions (id, platform, platform_session_id, updated_at) VALUES ('iso-newest', 'deepseek', 'iso', '2026-06-08T01:35:06.105+08:00')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO sessions (id, platform, platform_session_id, updated_at) VALUES ('older-epoch', 'deepseek', 'older', '1749317706.105')")
            .execute(&pool)
            .await
            .unwrap();

        let latest = sync_status(&pool, "deepseek").await.unwrap().unwrap();

        // The value must be the normalized epoch-seconds text used for
        // ordering, not the raw stored string.
        let parsed: f64 = latest.parse().unwrap_or_else(|error| {
            panic!("sync_status must return a parseable epoch string, got {latest}: {error}")
        });
        assert!((parsed - 1_780_853_706.105).abs() < 0.01, "got {latest}");
        assert!(
            sync_status(&pool, "missing-platform")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn delete_session_removes_all_session_vectors_via_batched_delete() {
        let pool = create_sync_pool().await;
        let session = session_fixture("session-vec-batch", "Batched vectors");
        import_sessions(&pool, std::slice::from_ref(&session), false)
            .await
            .unwrap();
        let session_id: String = sqlx::query_scalar(
            "SELECT id FROM sessions WHERE platform_session_id = 'session-vec-batch'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        for chunk_id in 201..=220_i64 {
            sqlx::query(
                "INSERT INTO embedding_chunks
                 (id, message_id, session_id, platform, chunk_index, role, text, content_hash,
                  backend_id, model_id, dim, status, updated_at)
                 VALUES (?, ?, ?, 'custom', ?, 'user', 'hello', 'h',
                         'local', 'model-a', 512, 'ready', 'now')",
            )
            .bind(chunk_id)
            .bind(format!("m{chunk_id}"))
            .bind(&session_id)
            .bind(chunk_id - 201)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO embedding_vec (chunk_id, embedding, session_id, message_id, platform) VALUES (?, ?, ?, 'm1', 'custom')",
            )
            .bind(chunk_id)
            .bind(vec![0u8; 512 * 4])
            .bind(&session_id)
            .execute(&pool)
            .await
            .unwrap();
        }
        assert_eq!(table_count(&pool, "embedding_vec").await, 20);

        delete_session(&pool, &session_id, false).await.unwrap();

        assert_eq!(table_count(&pool, "embedding_vec").await, 0);
        assert_eq!(table_count(&pool, "embedding_chunks").await, 0);
    }

    #[tokio::test]
    async fn reimport_deletes_stale_session_vectors_and_chunks() {
        let pool = create_sync_pool().await;
        let first = normalize_session(
            "custom",
            &json!({"id":"vec-reimport","title":"one","messages":[{"role":"user","content":"old"}]}),
        )
        .unwrap();
        import_sessions(&pool, &[first], false).await.unwrap();
        let session_id: String = sqlx::query_scalar("SELECT id FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        for chunk_id in 301..=304_i64 {
            sqlx::query(
                "INSERT INTO embedding_chunks
                 (id, message_id, session_id, platform, chunk_index, role, text, content_hash,
                  backend_id, model_id, dim, status, updated_at)
                 VALUES (?, ?, ?, 'custom', ?, 'user', 't', 'h',
                         'local', 'model-a', 512, 'ready', 'now')",
            )
            .bind(chunk_id)
            .bind(format!("m{chunk_id}"))
            .bind(&session_id)
            .bind(chunk_id - 301)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO embedding_vec (chunk_id, embedding, session_id, message_id, platform) VALUES (?, ?, ?, 'm', 'custom')",
            )
            .bind(chunk_id)
            .bind(vec![0u8; 512 * 4])
            .bind(&session_id)
            .execute(&pool)
            .await
            .unwrap();
        }
        assert_eq!(table_count(&pool, "embedding_vec").await, 4);

        let second = normalize_session(
            "custom",
            &json!({"id":"vec-reimport","title":"two","messages":[{"role":"user","content":"new"}]}),
        )
        .unwrap();
        import_sessions(&pool, &[second], false).await.unwrap();

        // The re-import cleared the previous chunks and their vectors in one
        // batched delete; no stale vector rows survive.
        assert_eq!(table_count(&pool, "embedding_chunks").await, 0);
        assert_eq!(table_count(&pool, "embedding_vec").await, 0);
        assert_eq!(table_count(&pool, "messages").await, 1);
    }
}
