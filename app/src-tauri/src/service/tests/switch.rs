use super::super::import_local_sessions;
use super::support::*;
use crate::models::{
    AppSettings, CloudBackendKind, CloudCredentialInput, CloudSyncSettings, NormalizedSession,
    S3CloudSyncSettings,
};
use crate::sync::backend::RemotePath;
use crate::sync::credentials::{
    SecretKind, SecretValue, StoredCloudCredentialProfile, load_credential_bundle,
};
use crate::sync::engine::HeadDocument;
use crate::sync::factory::backend_from_store;
use crate::sync::test_s3_server::TestS3;
use crate::sync::test_server::TestWebDav;
use crate::sync::types::MutationOperation;
use std::sync::Arc;
use tokio::sync::Notify;

#[tokio::test]
async fn backend_switch_defers_generation_replay_without_rewriting_local_versions() {
    let (service, _data_dir) = service_with_local_session_fixture().await;
    service.ensure_local_device().await.unwrap();
    service.sync_store.seed_local_baseline().await.unwrap();
    sqlx::query(
        "UPDATE sync_device_state SET hlc_wall_ms = ?, hlc_counter = ? WHERE singleton = 1",
    )
    .bind(i64::MAX)
    .bind(i64::MAX)
    .execute(&service.pool)
    .await
    .unwrap();
    service
        .credentials
        .set(
            "default",
            SecretKind::WebDavPassword,
            SecretValue::new("old-webdav-password"),
        )
        .await
        .unwrap();
    let settings_before = service.settings().await;
    let outbox_before = service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();
    let server = TestS3::start("AKID", Some("new-token")).await;
    let tested = service
        .test_cloud_sync_connection(
            CloudSyncSettings {
                backend: CloudBackendKind::S3,
                enabled: true,
                s3: S3CloudSyncSettings {
                    endpoint_url: server.endpoint().into(),
                    region: "us-east-1".into(),
                    bucket: "archive".into(),
                    prefix: "baseline-rollback".into(),
                    force_path_style: true,
                },
                ..CloudSyncSettings::default()
            },
            CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: Some("new-token".into()),
                sync_password: None,
            },
        )
        .await
        .unwrap();
    let draft_remote_id = tested.cloud_sync.remote_id.clone();

    let updated = service
        .update_settings_with_cloud_credentials(
            AppSettings {
                cloud_sync: tested.cloud_sync,
                ..settings_before.clone()
            },
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: Some("new-token".into()),
                sync_password: None,
            }),
        )
        .await
        .unwrap();

    assert_eq!(updated.cloud_sync.backend, CloudBackendKind::S3);
    assert_eq!(service.settings().await.cloud_sync, updated.cloud_sync);
    assert_eq!(
        service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        outbox_before
    );
    let previous_bundle = load_credential_bundle(service.credentials.as_ref(), "default")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        previous_bundle.active,
        StoredCloudCredentialProfile::Webdav {
            password: "old-webdav-password".into(),
            sync_passphrase: None
        }
    );
    let switched_bundle = load_credential_bundle(service.credentials.as_ref(), &draft_remote_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        switched_bundle.active,
        StoredCloudCredentialProfile::S3 {
            access_key_id: "AKID".into(),
            secret_access_key: "secret-key".into(),
            session_token: Some("new-token".into()),
            sync_passphrase: None
        }
    );
    assert!(switched_bundle.pending.is_none());
}

#[tokio::test]
async fn webdav_to_s3_switch_publishes_live_sessions_and_tombstones_without_touching_webdav() {
    let (service, _data_dir) = service_with_local_session_fixture().await;
    let webdav_server = TestWebDav::start("alice", "dav-password").await;
    let webdav_test = service
        .test_cloud_sync_connection(
            CloudSyncSettings {
                backend: CloudBackendKind::Webdav,
                enabled: true,
                base_url: webdav_server.endpoint().into(),
                root_path: String::new(),
                username: "alice".into(),
                ..CloudSyncSettings::default()
            },
            CloudCredentialInput::Webdav {
                password: "dav-password".into(),
                sync_password: None,
            },
        )
        .await
        .unwrap();
    let webdav_settings = service
        .update_settings_with_cloud_credentials(
            AppSettings {
                cloud_sync: webdav_test.cloud_sync,
                ..service.settings().await
            },
            Some(CloudCredentialInput::Webdav {
                password: "dav-password".into(),
                sync_password: None,
            }),
        )
        .await
        .unwrap();
    service
        .sync_once_locked(webdav_settings.clone())
        .await
        .unwrap();
    let webdav_backend =
        backend_from_store(&webdav_settings.cloud_sync, service.credentials.as_ref())
            .await
            .unwrap();
    let old_vault_before = webdav_backend
        .get(&RemotePath::parse("v1/vault.json").unwrap())
        .await
        .unwrap();

    import_local_sessions(
        &service.pool,
        &[NormalizedSession {
            id: "live-session".into(),
            platform: "fixture".into(),
            platform_session_id: "live-2".into(),
            title: "Live after switch".into(),
            created_at: None,
            updated_at: None,
            imported_at: "2026-08-05T00:00:00Z".into(),
            messages: Vec::new(),
            raw_data: serde_json::json!({"source": "switch-test"}),
        }],
    )
    .await
    .unwrap();
    service.delete("local-session").await.unwrap();
    let webdav_method_count_before_switch = webdav_server.methods().await.len();

    let s3_server = TestS3::start("AKID", None).await;
    let s3_test = service
        .test_cloud_sync_connection(
            CloudSyncSettings {
                backend: CloudBackendKind::S3,
                enabled: true,
                s3: S3CloudSyncSettings {
                    endpoint_url: s3_server.endpoint().into(),
                    region: "us-east-1".into(),
                    bucket: "archive".into(),
                    prefix: "switch-baseline".into(),
                    force_path_style: true,
                },
                ..CloudSyncSettings::default()
            },
            CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: None,
                sync_password: None,
            },
        )
        .await
        .unwrap();
    let s3_settings = service
        .update_settings_with_cloud_credentials(
            AppSettings {
                cloud_sync: s3_test.cloud_sync,
                ..service.settings().await
            },
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: None,
                sync_password: None,
            }),
        )
        .await
        .unwrap();
    service.sync_once_locked(s3_settings.clone()).await.unwrap();

    assert_eq!(
        webdav_server.methods().await.len(),
        webdav_method_count_before_switch
    );
    assert_eq!(
        service.sync_store.pending_mutation_count().await.unwrap(),
        0
    );
    let old_vault_after = webdav_backend
        .get(&RemotePath::parse("v1/vault.json").unwrap())
        .await
        .unwrap();
    assert_eq!(old_vault_after.bytes, old_vault_before.bytes);
    assert_eq!(old_vault_after.etag, old_vault_before.etag);

    let s3_backend = backend_from_store(&s3_settings.cloud_sync, service.credentials.as_ref())
        .await
        .unwrap();
    let device = service.ensure_local_device().await.unwrap();
    let head_path = RemotePath::parse(&format!(
        "v1/generations/{}/devices/{}/head.json",
        s3_settings.cloud_sync.generation_id, device.device_id
    ))
    .unwrap();
    let mut current: Option<HeadDocument> =
        Some(serde_json::from_slice(&s3_backend.get(&head_path).await.unwrap().bytes).unwrap());
    let mut operations = Vec::new();
    while let Some(head) = current {
        let decoded = crate::sync::bundle::open_bundle(
            &s3_backend
                .get(&RemotePath::parse(&head.path).unwrap())
                .await
                .unwrap()
                .bytes,
            &crate::sync::bundle::BundleLimits::default(),
        )
        .unwrap();
        operations.extend(decoded.contents.changes.iter().map(|change| {
            (
                change.key.platform.clone(),
                change.key.platform_session_id.clone(),
                change.operation.clone(),
            )
        }));
        current = match (
            decoded.header.previous_path,
            decoded.header.previous_sha256,
            decoded.header.previous_end_seq,
        ) {
            (Some(path), Some(sha256), Some(end_seq)) => Some(HeadDocument {
                generation_id: s3_settings.cloud_sync.generation_id.clone(),
                device_id: device.device_id.clone(),
                end_seq,
                path,
                sha256,
            }),
            (None, None, None) => None,
            _ => panic!("incomplete S3 baseline chain"),
        };
    }
    assert!(operations.contains(&("fixture".into(), "live-2".into(), MutationOperation::Upsert,)));
    assert!(operations.contains(&(
        "fixture".into(),
        "local-1".into(),
        MutationOperation::Delete,
    )));
}

#[tokio::test]
async fn backend_switch_waits_for_running_sync_before_changing_configuration() {
    let (service, _data_dir) = service_with_local_session_fixture().await;
    let running_sync = service.sync_gate.lock().await;
    let mut next = service.settings().await;
    next.cloud_sync.backend = CloudBackendKind::S3;
    next.cloud_sync.s3.bucket = "new-backend".into();

    let transition_started = Arc::new(Notify::new());
    let mut transition = tokio::spawn({
        let service = service.clone();
        let transition_started = transition_started.clone();
        async move {
            transition_started.notify_one();
            service
                .update_settings_with_cloud_credentials(next, None)
                .await
        }
    });
    transition_started.notified().await;

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut transition)
            .await
            .is_err(),
        "backend switch completed while the sync gate was still held"
    );
    assert_eq!(
        service.settings().await.cloud_sync.backend,
        CloudBackendKind::Webdav
    );
    assert!(
        service
            .sync_store
            .pending_mutations(10)
            .await
            .unwrap()
            .is_empty()
    );

    drop(running_sync);
    let updated = tokio::time::timeout(std::time::Duration::from_secs(10), &mut transition)
        .await
        .expect("backend switch remained blocked after the running sync completed")
        .unwrap()
        .unwrap();

    assert_eq!(updated.cloud_sync.backend, CloudBackendKind::S3);
    assert!(
        service
            .sync_store
            .pending_mutations(10)
            .await
            .unwrap()
            .is_empty()
    );
}
