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

// The three tests below exercise `CloudSyncWorkerState`, the layer that wraps
// the lower-level `SchedulerState` (tested exhaustively in
// `sync::engine::tests::scheduler_coalesces_by_priority_and_caps_backoff`,
// including priority coalescing and the exact backoff cap sequence) with
// absolute due-time bookkeeping (`PendingCloudSync::due`, `pending_delay`,
// `take_due`) and jittered retry scheduling. They intentionally avoid
// re-asserting backoff numbers already covered at that lower level and focus
// on what only this layer adds.

#[test]
fn production_scheduler_computes_due_time_from_coalesced_priority() {
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
fn production_scheduler_rejects_submits_while_paused_and_unpauses_on_manual() {
    let now = tokio::time::Instant::now();
    let mut worker = CloudSyncWorkerState::default();

    worker.submit(SyncTrigger::Periodic, now);
    let trigger = worker
        .take_due(now + std::time::Duration::from_secs(15 * 60))
        .expect("periodic trigger should become runnable");
    worker.failure(trigger, now, true, false, 17);
    assert!(worker.scheduler.paused_for_auth);
    // Worker-level behavior: an auth failure clears the pending due time and
    // `submit` reports `false` (nothing scheduled) for non-Manual triggers.
    assert_eq!(worker.pending_trigger(), None);
    assert!(!worker.submit(SyncTrigger::Periodic, now));

    assert!(worker.submit(SyncTrigger::Manual, now));
    assert!(!worker.scheduler.paused_for_auth);
    assert_eq!(worker.pending_delay(now), Some(std::time::Duration::ZERO));
}

#[test]
fn production_scheduler_widens_jittered_due_time_on_repeated_offline_failures() {
    let now = tokio::time::Instant::now();
    let mut worker = CloudSyncWorkerState::default();

    worker.submit(SyncTrigger::Startup, now);
    let trigger = worker
        .take_due(now + std::time::Duration::from_secs(30))
        .expect("startup trigger should become runnable");
    worker.failure(trigger, now, false, true, 0);
    let retry = worker
        .pending_delay(now)
        .expect("offline failure should schedule a retry due time");
    assert!(retry >= std::time::Duration::from_secs(48));
    assert!(retry <= std::time::Duration::from_secs(72));

    // A second offline failure doubles the underlying backoff, so the jitter
    // window (and therefore the due-time bounds) widens accordingly. This is
    // `CloudSyncWorkerState`-specific: the lower-level scheduler has no
    // concept of an absolute due time or jitter at all.
    let trigger = worker
        .take_due(now + retry)
        .expect("retry should become runnable once due");
    worker.failure(trigger, now, false, true, 0);
    let second_retry = worker
        .pending_delay(now)
        .expect("second offline failure should reschedule with a wider jitter window");
    assert!(second_retry >= std::time::Duration::from_secs(96));
    assert!(second_retry <= std::time::Duration::from_secs(144));

    // A non-retryable failure clears the pending due time entirely, regardless
    // of the accumulated backoff (whose cap is verified at the scheduler
    // level, not here).
    let trigger = worker
        .take_due(now + second_retry)
        .expect("second retry should become runnable once due");
    worker.failure(trigger, now, false, false, 0);
    assert_eq!(worker.pending_trigger(), None);
}
