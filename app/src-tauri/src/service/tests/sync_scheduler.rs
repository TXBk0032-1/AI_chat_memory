use super::super::{CloudSyncWorkerState, classify_cloud_error, map_cloud_error};
use crate::models::CloudSyncState;
use crate::sync::backend::{CloudError, CloudErrorKind};
use crate::sync::types::SyncTrigger;

#[test]
fn cloud_error_kinds_map_to_distinct_runtime_states() {
    let cases = [
        (CloudErrorKind::Auth, CloudSyncState::AuthError, "auth"),
        (CloudErrorKind::Offline, CloudSyncState::Offline, "offline"),
        (
            CloudErrorKind::Precondition,
            CloudSyncState::ProtocolError,
            "precondition",
        ),
        (
            CloudErrorKind::Protocol,
            CloudSyncState::ProtocolError,
            "protocol",
        ),
    ];
    for (kind, expected_state, expected_code) in cases {
        let error = map_cloud_error(CloudError::new(kind, "fixture cloud error"));
        assert_eq!(
            classify_cloud_error(&error),
            (expected_state, expected_code)
        );
    }
    assert_eq!(
        classify_cloud_error(&crate::error::AppError::Credential("missing".into())),
        (CloudSyncState::NeedsUnlock, "needs_unlock")
    );
}

#[test]
fn production_scheduler_coalesces_priority_and_uses_bounded_delays() {
    let now = tokio::time::Instant::now();
    let mut worker = CloudSyncWorkerState::default();

    assert!(worker.submit(SyncTrigger::Periodic, now));
    assert_eq!(
        worker.pending_delay(now),
        Some(std::time::Duration::from_secs(15 * 60))
    );
    assert!(worker.submit(SyncTrigger::Startup, now));
    assert!(worker.submit(SyncTrigger::LocalMutation, now));
    assert_eq!(worker.pending_trigger(), Some(SyncTrigger::LocalMutation));
    assert_eq!(
        worker.pending_delay(now),
        Some(std::time::Duration::from_secs(5))
    );

    assert!(worker.submit(SyncTrigger::Manual, now));
    assert_eq!(worker.pending_trigger(), Some(SyncTrigger::Manual));
    assert_eq!(worker.pending_delay(now), Some(std::time::Duration::ZERO));
}

#[test]
fn production_scheduler_pauses_auth_retries_until_manual_trigger() {
    let now = tokio::time::Instant::now();
    let mut worker = CloudSyncWorkerState::default();

    worker.submit(SyncTrigger::Periodic, now);
    let trigger = worker
        .take_due(now + std::time::Duration::from_secs(15 * 60))
        .expect("periodic trigger should become runnable");
    worker.failure(trigger, now, true, false, 17);
    assert!(worker.scheduler.paused_for_auth);
    assert_eq!(worker.pending_trigger(), None);
    assert!(!worker.submit(SyncTrigger::Periodic, now));

    assert!(worker.submit(SyncTrigger::Manual, now));
    assert!(!worker.scheduler.paused_for_auth);
    assert_eq!(worker.pending_delay(now), Some(std::time::Duration::ZERO));
}

#[test]
fn production_scheduler_retries_only_offline_with_capped_jitter() {
    let now = tokio::time::Instant::now();
    let mut worker = CloudSyncWorkerState::default();

    worker.submit(SyncTrigger::Startup, now);
    let trigger = worker
        .take_due(now + std::time::Duration::from_secs(30))
        .expect("startup trigger should become runnable");
    worker.failure(trigger, now, false, true, 0);
    let retry = worker
        .pending_delay(now)
        .expect("offline failure should schedule a retry");
    assert!(retry >= std::time::Duration::from_secs(48));
    assert!(retry <= std::time::Duration::from_secs(72));

    for _ in 0..10 {
        let trigger = worker
            .take_due(now + std::time::Duration::from_secs(2 * 60 * 60))
            .expect("retry should become runnable");
        worker.failure(trigger, now, false, true, 0);
    }
    assert_eq!(
        worker.scheduler.retry_delay,
        std::time::Duration::from_secs(60 * 60)
    );

    let trigger = worker
        .take_due(now + std::time::Duration::from_secs(2 * 60 * 60))
        .expect("capped retry should become runnable");
    worker.failure(trigger, now, false, false, 0);
    assert_eq!(worker.pending_trigger(), None);
}
