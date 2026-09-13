use super::support::*;
use crate::models::ImportRequest;
use std::sync::Arc;
use tokio::sync::Notify;

#[tokio::test]
async fn import_waits_for_generation_maintenance_before_mutating_local_state() {
    let (service, _data_dir) = service_with_local_session_fixture().await;
    let maintenance = service.sync_gate.lock().await;
    let started = Arc::new(Notify::new());
    let mut operation = tokio::spawn({
        let service = service.clone();
        let started = started.clone();
        async move {
            started.notify_one();
            service
                .import(ImportRequest {
                    platform: "fixture".into(),
                    sessions: vec![serde_json::json!({
                        "id": "local-2",
                        "title": "Imported during maintenance",
                        "messages": []
                    })],
                })
                .await
        }
    });
    started.notified().await;

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut operation)
            .await
            .is_err(),
        "import completed while generation maintenance held the gate"
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&service.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    drop(maintenance);
    tokio::time::timeout(std::time::Duration::from_secs(2), &mut operation)
        .await
        .expect("import remained blocked after maintenance finished")
        .unwrap()
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&service.pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn delete_waits_for_generation_maintenance_before_mutating_local_state() {
    let (service, _data_dir) = service_with_local_session_fixture().await;
    let maintenance = service.sync_gate.lock().await;
    let started = Arc::new(Notify::new());
    let mut operation = tokio::spawn({
        let service = service.clone();
        let started = started.clone();
        async move {
            started.notify_one();
            service.delete("local-session").await
        }
    });
    started.notified().await;

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut operation)
            .await
            .is_err(),
        "delete completed while generation maintenance held the gate"
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&service.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    drop(maintenance);
    tokio::time::timeout(std::time::Duration::from_secs(2), &mut operation)
        .await
        .expect("delete remained blocked after maintenance finished")
        .unwrap()
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&service.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
