//! MCP 测试夹具：真实 AppService + 种子会话。
//! 语义后端固定 Ollama，构造期零网络、零 CUDA 加载。

use crate::database;
use crate::models::EmbeddingBackendKind;
use crate::service::AppService;
use crate::settings::SettingsStore;
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 打开临时库并写入种子会话（含 keyword 检索依赖的 FTS 行）。
pub(crate) async fn seeded_pool(data_dir: &Path) -> SqlitePool {
    let pool = database::connect(&data_dir.join("chat_memory.db"))
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO sessions (id, platform, platform_session_id, title, raw_data)
         VALUES ('seed-session', 'deepseek', 'seed-1', 'Rust 异步编程讨论', '{}')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, metadata, seq)
         VALUES ('seed-m1', 'seed-session', 'user', '如何理解 Rust 的 async/await？', '{}', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, metadata, seq)
         VALUES ('seed-m2', 'seed-session', 'assistant', 'async/await 是零开销抽象。', '{}', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    // keyword 检索走 session_fts（仅 import 路径维护），夹具需手动补 FTS 行。
    sqlx::query("INSERT INTO session_fts_ids(session_id) VALUES ('seed-session')")
        .execute(&pool)
        .await
        .unwrap();
    let fts_rowid: i64 =
        sqlx::query_scalar("SELECT fts_rowid FROM session_fts_ids WHERE session_id = ?")
            .bind("seed-session")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("INSERT INTO session_fts(rowid, session_id, title, content) VALUES (?, ?, ?, ?)")
        .bind(fts_rowid)
        .bind("seed-session")
        .bind("Rust 异步编程讨论")
        .bind("如何理解 Rust 的 async/await？\nasync/await 是零开销抽象。")
        .execute(&pool)
        .await
        .unwrap();
    pool
}

/// 独立临时目录中的完整 AppService，返回值附带数据目录供清理/检查。
pub(crate) async fn test_app_service() -> (AppService, PathBuf) {
    let data_dir =
        std::env::temp_dir().join(format!("ai-chat-memory-mcp-test-{}", uuid::Uuid::new_v4()));
    let pool = seeded_pool(&data_dir).await;
    let settings = Arc::new(
        SettingsStore::load(data_dir.join("settings.json"))
            .await
            .unwrap(),
    );
    let mut settings_value = settings.get().await;
    settings_value.semantic_search.backend = EmbeddingBackendKind::Ollama;
    settings_value.mcp_enabled = true;
    settings.update(settings_value.clone()).await.unwrap();
    let service = AppService::new(pool, settings, data_dir.clone())
        .await
        .unwrap();
    (service, data_dir)
}
