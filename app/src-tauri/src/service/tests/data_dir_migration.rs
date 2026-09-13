use super::super::AppService;
use super::support::*;
use crate::database;
use crate::error::AppError;
use crate::models::{EmbeddingBackendKind, ImportRequest};
use crate::settings::SettingsStore;
use std::sync::Arc;

#[tokio::test]
async fn move_data_directory_rejects_writes_until_restart() {
    // After the DB is snapshotted to the new directory and a restart is
    // scheduled, the service must reject all subsequent writes (import/delete/
    // sync) so nothing mutates the old pool before the process actually
    // restarts.
    let (service, data_dir) = service_with_local_session_fixture().await;
    let destination =
        std::env::temp_dir().join(format!("ai-chat-memory-move-{}", uuid::Uuid::new_v4()));

    service.move_data_directory(&destination).await.unwrap();
    assert!(
        destination.join("chat_memory.db").exists(),
        "snapshot was written"
    );

    // Subsequent local writes must be rejected, not silently executed.
    let delete_err = service.delete("local-session").await.unwrap_err();
    assert!(
        matches!(delete_err, AppError::Cancelled(_)),
        "delete after move must be Cancelled, got {:?}",
        delete_err
    );

    let import_err = service
        .import(ImportRequest {
            platform: "fixture".into(),
            sessions: vec![serde_json::json!({
                "id": "local-2",
                "title": "after move",
                "messages": []
            })],
        })
        .await
        .unwrap_err();
    assert!(
        matches!(import_err, AppError::Cancelled(_)),
        "import after move must be Cancelled, got {:?}",
        import_err
    );

    // A manual sync must not run either; sync_now falls back to sync_now_direct
    // when the scheduler channel is closed (for_tests), so it must surface the
    // same shutdown rejection rather than touching the old pool.
    let sync_err = service.sync_now().await.unwrap_err();
    assert!(
        matches!(sync_err, AppError::Cancelled(_)),
        "sync_now after move must be Cancelled, got {:?}",
        sync_err
    );

    // The original session row is still present in the old pool (nothing wrote
    // through), proving the rejection happened before any mutation.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&service.pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "no writes should have reached the old pool");

    let _ = std::fs::remove_dir_all(&destination);
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn move_data_directory_rechecks_the_destination_inside_the_sync_gate() {
    // 目标存在性检查必须与 VACUUM 同处 sync_gate 临界区内。并发竞争者
    // 在等锁期间创建了目标文件时，加锁后的复检必须给出明确的配置错误，
    // 而不是让 VACUUM INTO 以晦涩的 SQLite 错误失败。
    let (service, data_dir) = service_with_local_session_fixture().await;
    let destination =
        std::env::temp_dir().join(format!("ai-chat-memory-move-race-{}", uuid::Uuid::new_v4()));

    // 先占用 sync_gate，让移动任务阻塞在临界区入口。
    let gate_guard = service.sync_gate.lock().await;
    let gated_service = service.clone();
    let gated_destination = destination.clone();
    let mover =
        tokio::spawn(async move { gated_service.move_data_directory(&gated_destination).await });
    // 等待移动任务阻塞在 sync_gate 上，再模拟竞争者写入目标文件。
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    tokio::fs::create_dir_all(&destination).await.unwrap();
    tokio::fs::write(destination.join("chat_memory.db"), b"competing")
        .await
        .unwrap();
    drop(gate_guard);
    let error = mover
        .await
        .expect("move task must not panic")
        .expect_err("the destination was created while the move waited on the sync gate");

    assert!(
        matches!(error, AppError::Configuration(ref message) if message.contains("已存在")),
        "the recheck inside the sync gate must surface the friendly conflict, got {error:?}"
    );

    let _ = std::fs::remove_dir_all(&destination);
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn move_data_directory_publishes_redirect_for_old_readers() {
    // 迁移完成后，旧数据目录必须留下原子 redirect marker：仍持有旧
    // SQLite 池的 MCP stdio 进程据此立即拒绝读取并提示重启，而不是继续
    // 服务已被搬走的陈旧数据。新目录自身没有 marker，重启后一切如常。
    let (service, data_dir) = service_with_local_session_fixture().await;
    let destination = std::env::temp_dir().join(format!(
        "ai-chat-memory-move-marker-{}",
        uuid::Uuid::new_v4()
    ));

    // 迁移前旧目录没有任何 marker，guard 必须放行。
    service.ensure_current_data_directory().await.unwrap();

    service.move_data_directory(&destination).await.unwrap();

    // 旧目录发布了 marker，旧池上的 guard 必须以 Cancelled 拒绝并提示重启，
    // 且报错携带 marker 里的 destination_hint，用户无需去旧目录翻 marker。
    let guard_error = service.ensure_current_data_directory().await.unwrap_err();
    assert!(
        matches!(guard_error, AppError::Cancelled(ref message)
            if message.contains("数据目录已迁移，请重启 MCP")
                && message.contains(destination.to_string_lossy().as_ref())),
        "guard after a successful move must be Cancelled with the restart hint and the destination, got {guard_error:?}"
    );
    let marker_path = data_dir.join(crate::data_directory_marker::DATA_DIRECTORY_REDIRECT_FILE);
    assert!(
        marker_path.is_file(),
        "the old data directory must carry the redirect marker"
    );

    // 目的目录确实拿到了数据库快照。
    assert!(
        destination.join("chat_memory.db").exists(),
        "the destination must hold the snapshotted database"
    );

    // 重启后的进程在目的目录构建 service，guard 必须放行（新目录无 marker）。
    let restarted_pool = database::connect(&destination.join("chat_memory.db"))
        .await
        .unwrap();
    let restarted_settings = Arc::new(
        SettingsStore::load(destination.join("settings.json"))
            .await
            .unwrap(),
    );
    let mut restarted_settings_value = restarted_settings.get().await;
    restarted_settings_value.semantic_search.backend = EmbeddingBackendKind::Ollama;
    restarted_settings
        .update(restarted_settings_value.clone())
        .await
        .unwrap();
    let restarted = AppService::new(restarted_pool, restarted_settings, destination.clone())
        .await
        .unwrap();
    restarted
        .ensure_current_data_directory()
        .await
        .expect("a service built on the destination directory must pass the guard");

    let _ = std::fs::remove_dir_all(&destination);
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn move_data_directory_leaves_no_marker_when_the_move_fails() {
    // 迁移失败（目的目录已有 chat_memory.db）时旧目录绝不能留下 marker：
    // 一旦发布，仍在运行的 MCP 进程会立刻拒绝读取，而数据库实际上
    // 并没有搬走，用户只会看到无来由的故障。
    let (service, data_dir) = service_with_local_session_fixture().await;
    let destination =
        std::env::temp_dir().join(format!("ai-chat-memory-move-fail-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&destination).await.unwrap();
    tokio::fs::write(destination.join("chat_memory.db"), b"competing")
        .await
        .unwrap();

    let error = service.move_data_directory(&destination).await.unwrap_err();
    assert!(
        matches!(error, AppError::Configuration(ref message) if message.contains("已存在")),
        "the competing database must surface the friendly configuration error, got {error:?}"
    );

    let marker_path = data_dir.join(crate::data_directory_marker::DATA_DIRECTORY_REDIRECT_FILE);
    assert!(
        !marker_path.exists(),
        "a failed move must not publish the redirect marker"
    );
    // 失败后旧目录的 guard 仍然放行，服务继续可用。
    service
        .ensure_current_data_directory()
        .await
        .expect("the old directory stays current after a failed move");

    let _ = std::fs::remove_dir_all(&destination);
    let _ = std::fs::remove_dir_all(&data_dir);
}
