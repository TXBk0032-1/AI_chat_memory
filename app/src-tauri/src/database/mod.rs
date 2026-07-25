pub mod connection;
mod details;
mod imports;
mod maintenance;
pub mod sessions;
pub mod timestamp;

pub use connection::{connect, copy_database};
pub use details::{get_session_branches, get_session_messages, open_session, search_session_hits};
pub use imports::import_sessions;
pub use maintenance::{delete_session, sync_status};
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
    use crate::normalizer::normalize_session;
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
        import_sessions(&pool, &[first]).await.unwrap();
        let original: String = sqlx::query_scalar("SELECT id FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        let second = normalize_session(
            "custom",
            &json!({"id":"a","title":"two","messages":[{"role":"assistant","content":"new"}]}),
        )
        .unwrap();
        import_sessions(&pool, &[second]).await.unwrap();
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
        import_sessions(&pool, &[session]).await.unwrap();
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
        delete_session(&pool, &id).await.unwrap();
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
}
