use super::super::{AppService, CloudSyncRuntime, CloudSyncScheduler, ServiceRole};
use super::support::*;
use crate::error::AppError;
use crate::models::{
    ApiStatus, CloudBackendKind, CloudCredentialInput, CloudSyncSettings, S3CloudSyncSettings,
};
use crate::settings::SettingsStore;
use crate::sync::backend::RemotePath;
use crate::sync::credentials::{
    CredentialStore, CredentialTransitionPhase, MemoryCredentialStore, PendingCredentialProfile,
    SecretKind, SecretValue, StoredCloudCredentialProfile, StoredCredentialBundle,
    load_credential_bundle, save_credential_bundle,
};
use crate::sync::engine::HeadDocument;
use crate::sync::factory::backend_from_store;
use crate::sync::store::SyncStore;
use crate::sync::test_s3_server::TestS3;
use crate::sync::vault::{
    HeadPublishRequest, VaultDocument, VaultIdentity, VaultProtection, VaultState,
    begin_generation_freeze_owned, begin_head_publish, load_or_create_vault,
    load_versioned_identity,
};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

#[tokio::test]
async fn restart_reconciles_a_committed_generation_before_selecting_credentials() {
    let memory = MemoryCredentialStore::default();
    let credentials: Arc<dyn CredentialStore> = Arc::new(memory.clone());
    let (service, _server, settings_before, backend, data_dir) =
        configured_encrypted_s3_service(credentials).await;
    let updated = service
        .update_settings_with_cloud_credentials(
            settings_before.clone(),
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: Some("old-token".into()),
                sync_password: Some("new-passphrase".into()),
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        load_versioned_identity(backend.as_ref())
            .await
            .unwrap()
            .identity
            .generation_id,
        updated.cloud_sync.generation_id
    );

    let mut crashed_bundle = StoredCredentialBundle::new(StoredCloudCredentialProfile::S3 {
        access_key_id: "AKID".into(),
        secret_access_key: "secret-key".into(),
        session_token: Some("old-token".into()),
        sync_passphrase: Some("old-passphrase".into()),
    });
    crashed_bundle
        .stage_transition(PendingCredentialProfile {
            credentials: StoredCloudCredentialProfile::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: Some("old-token".into()),
                sync_passphrase: Some("new-passphrase".into()),
            },
            operation_id: "rotation-crash-recovery".into(),
            target_vault_id: updated.cloud_sync.vault_id.clone(),
            target_generation_id: updated.cloud_sync.generation_id.clone(),
            phase: CredentialTransitionPhase::Prepared,
        })
        .unwrap();
    save_credential_bundle(
        &memory,
        &settings_before.cloud_sync.remote_id,
        &crashed_bundle,
    )
    .await
    .unwrap();
    service
        .settings
        .update(settings_before.clone())
        .await
        .unwrap();

    let reloaded_settings = Arc::new(
        SettingsStore::load(data_dir.join("settings.json"))
            .await
            .unwrap(),
    );
    let restarted = AppService {
        pool: service.pool.clone(),
        settings: reloaded_settings,
        semantic: service.semantic.clone(),
        role: ServiceRole::Desktop,
        data_dir: Arc::new(data_dir.clone()),
        api_status: Arc::new(RwLock::new(ApiStatus::Starting)),
        last_userscript_request_at: Arc::new(RwLock::new(None)),
        sync_store: SyncStore::new(service.pool.clone()),
        credentials: Arc::new(memory.clone()),
        cloud_sync_scheduler: CloudSyncScheduler::for_tests(),
        sync_gate: Arc::new(Mutex::new(())),
        cloud_sync_runtime: Arc::new(RwLock::new(CloudSyncRuntime::default())),
        shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    restarted
        .sync_once_locked(restarted.settings().await)
        .await
        .expect("restart should converge the pending credential transition");

    let persisted = restarted.settings().await;
    assert_eq!(
        persisted.cloud_sync.generation_id,
        updated.cloud_sync.generation_id
    );
    let reconciled =
        crate::sync::credentials::load_credential_bundle(&memory, &persisted.cloud_sync.remote_id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(reconciled.active.sync_passphrase(), Some("new-passphrase"));
    assert!(reconciled.pending.is_none());
}

#[tokio::test]
async fn restart_rolls_back_an_expired_pending_building_freeze() {
    let (mut service, _data_dir) = service_with_local_session_fixture().await;
    let credentials = MemoryCredentialStore::default();
    service.credentials = Arc::new(credentials.clone());
    let server = TestS3::start("AKID", None).await;
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: "remote-expired-pending-freeze".into(),
        vault_id: "vault-expired-pending-freeze".into(),
        generation_id: "generation-old".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "expired-pending-freeze".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    let active_profile = StoredCloudCredentialProfile::S3 {
        access_key_id: "AKID".into(),
        secret_access_key: "secret-key".into(),
        session_token: None,
        sync_passphrase: None,
    };
    let mut bundle = StoredCredentialBundle::new(active_profile.clone());
    bundle
        .stage_transition(PendingCredentialProfile {
            credentials: active_profile,
            operation_id: "rotation-expired-freeze".into(),
            target_vault_id: settings.cloud_sync.vault_id.clone(),
            target_generation_id: "generation-next".into(),
            phase: CredentialTransitionPhase::Prepared,
        })
        .unwrap();
    save_credential_bundle(&credentials, &settings.cloud_sync.remote_id, &bundle)
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, &credentials)
        .await
        .unwrap();
    let active = VaultDocument::active(
        VaultIdentity {
            format_version: 2,
            vault_id: settings.cloud_sync.vault_id.clone(),
            generation_id: settings.cloud_sync.generation_id.clone(),
        },
        VaultProtection::plain(),
    );
    load_or_create_vault(backend.as_ref(), active.clone())
        .await
        .unwrap();
    begin_generation_freeze_owned(
        backend.as_ref(),
        &active,
        "generation-next",
        VaultProtection::plain(),
        "rotation-expired-freeze",
        "device-restart",
        1,
        2,
    )
    .await
    .unwrap();

    let reconciled = service
        .reconcile_pending_credential_transition(settings.clone())
        .await
        .expect("an expired building freeze should safely roll back");

    assert_eq!(reconciled.cloud_sync.generation_id, "generation-old");
    let remote = load_versioned_identity(backend.as_ref()).await.unwrap();
    assert_eq!(remote.state, VaultState::Active);
    assert_eq!(remote.identity.generation_id, "generation-old");
    let stored = load_credential_bundle(&credentials, &settings.cloud_sync.remote_id)
        .await
        .unwrap()
        .unwrap();
    assert!(stored.pending.is_none());
}

#[tokio::test]
async fn restart_activates_an_expired_pending_ready_freeze() {
    let (mut service, _data_dir) = service_with_local_session_fixture().await;
    let credentials = MemoryCredentialStore::default();
    service.credentials = Arc::new(credentials.clone());
    let server = TestS3::start("AKID", None).await;
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: "remote-expired-ready-freeze".into(),
        vault_id: "vault-expired-ready-freeze".into(),
        generation_id: "generation-old".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "expired-ready-freeze".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    let profile = StoredCloudCredentialProfile::S3 {
        access_key_id: "AKID".into(),
        secret_access_key: "secret-key".into(),
        session_token: None,
        sync_passphrase: None,
    };
    let mut bundle = StoredCredentialBundle::new(profile.clone());
    bundle
        .stage_transition(PendingCredentialProfile {
            credentials: profile,
            operation_id: "rotation-ready-freeze".into(),
            target_vault_id: settings.cloud_sync.vault_id.clone(),
            target_generation_id: "generation-next".into(),
            phase: CredentialTransitionPhase::RemoteFrozen,
        })
        .unwrap();
    save_credential_bundle(&credentials, &settings.cloud_sync.remote_id, &bundle)
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, &credentials)
        .await
        .unwrap();
    let active = VaultDocument::active(
        VaultIdentity {
            format_version: 2,
            vault_id: settings.cloud_sync.vault_id.clone(),
            generation_id: settings.cloud_sync.generation_id.clone(),
        },
        VaultProtection::plain(),
    );
    load_or_create_vault(backend.as_ref(), active.clone())
        .await
        .unwrap();
    let current = load_versioned_identity(backend.as_ref()).await.unwrap();
    let frozen = VaultDocument {
        identity: active.identity,
        protection: active.protection,
        compatibility: None,
        state: VaultState::Frozen {
            operation_id: "rotation-ready-freeze".into(),
            owner_device_id: "device-restart".into(),
            started_at_ms: 1,
            lease_expires_at_ms: 2,
            target_generation_id: "generation-next".into(),
            target_protection: VaultProtection::plain(),
            stage: crate::sync::vault::GenerationMaintenanceStage::ReadyToActivate,
            retire_released_v1_compatibility: false,
        },
    };
    backend
        .put_if_match(
            &RemotePath::parse("v1/vault.json").unwrap(),
            &serde_json::to_vec(&frozen).unwrap(),
            &current.etag,
        )
        .await
        .unwrap();

    let reconciled = service
        .reconcile_pending_credential_transition(settings.clone())
        .await
        .expect("an expired ready freeze should activate the prepared generation");

    assert_eq!(reconciled.cloud_sync.generation_id, "generation-next");
    let remote = load_versioned_identity(backend.as_ref()).await.unwrap();
    assert_eq!(remote.state, VaultState::Active);
    assert_eq!(remote.identity.generation_id, "generation-next");
    let stored = load_credential_bundle(&credentials, &settings.cloud_sync.remote_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.active, bundle.pending.clone().unwrap().credentials);
    assert!(stored.pending.is_none());
}

#[tokio::test]
async fn restart_finishes_a_pending_head_publication_before_reconciling_credentials() {
    let (mut service, _data_dir) = service_with_local_session_fixture().await;
    let credentials = MemoryCredentialStore::default();
    service.credentials = Arc::new(credentials.clone());
    let server = TestS3::start("AKID", None).await;
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: "remote-publishing-restart".into(),
        vault_id: "vault-publishing-restart".into(),
        generation_id: "generation-old".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "publishing-restart".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    let profile = StoredCloudCredentialProfile::S3 {
        access_key_id: "AKID".into(),
        secret_access_key: "secret-key".into(),
        session_token: None,
        sync_passphrase: None,
    };
    let mut bundle = StoredCredentialBundle::new(profile.clone());
    bundle
        .stage_transition(PendingCredentialProfile {
            credentials: profile,
            operation_id: "rotation-after-publish".into(),
            target_vault_id: settings.cloud_sync.vault_id.clone(),
            target_generation_id: "generation-next".into(),
            phase: CredentialTransitionPhase::Prepared,
        })
        .unwrap();
    save_credential_bundle(&credentials, &settings.cloud_sync.remote_id, &bundle)
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, &credentials)
        .await
        .unwrap();
    load_or_create_vault(
        backend.as_ref(),
        VaultDocument::active(
            VaultIdentity {
                format_version: 2,
                vault_id: settings.cloud_sync.vault_id.clone(),
                generation_id: settings.cloud_sync.generation_id.clone(),
            },
            VaultProtection::plain(),
        ),
    )
    .await
    .unwrap();
    let active = load_versioned_identity(backend.as_ref()).await.unwrap();
    let head_path = format!(
        "v1/generations/{}/devices/device-restart/head.json",
        settings.cloud_sync.generation_id
    );
    let head = HeadDocument {
        generation_id: settings.cloud_sync.generation_id.clone(),
        device_id: "device-restart".into(),
        end_seq: 1,
        path: format!(
            "v1/generations/{}/devices/device-restart/bundles/1-1-test.acmb",
            settings.cloud_sync.generation_id
        ),
        sha256: "test".into(),
    };
    begin_head_publish(
        backend.as_ref(),
        &active,
        HeadPublishRequest {
            operation_id: "publish-restart".into(),
            owner_device_id: "device-restart".into(),
            started_at_ms: 1,
            lease_expires_at_ms: 2,
            head_path: head_path.clone(),
            expected_head_etag: None,
            replacement_head_json: serde_json::to_string(&head).unwrap(),
            published_mutation_count: 1,
        },
    )
    .await
    .unwrap();

    let reconciled = service
        .reconcile_pending_credential_transition(settings.clone())
        .await
        .expect("a stored head publication should finish deterministically");

    assert_eq!(reconciled.cloud_sync.generation_id, "generation-old");
    assert_eq!(
        load_versioned_identity(backend.as_ref())
            .await
            .unwrap()
            .state,
        VaultState::Active
    );
    assert_eq!(
        backend
            .get(&RemotePath::parse(&head_path).unwrap())
            .await
            .unwrap()
            .bytes,
        serde_json::to_vec(&head).unwrap()
    );
    let stored = load_credential_bundle(&credentials, &settings.cloud_sync.remote_id)
        .await
        .unwrap()
        .unwrap();
    assert!(stored.pending.is_none());
}

#[tokio::test]
async fn expired_pending_freeze_with_a_wrong_active_passphrase_does_not_touch_remote_state() {
    let (mut service, _data_dir) = service_with_local_session_fixture().await;
    let credentials = MemoryCredentialStore::default();
    service.credentials = Arc::new(credentials.clone());
    let server = TestS3::start("AKID", None).await;
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        encryption_enabled: true,
        remote_id: "remote-fresh-wrong-passphrase".into(),
        vault_id: "vault-fresh-wrong-passphrase".into(),
        generation_id: "generation-old".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "fresh-wrong-passphrase".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    let profile = StoredCloudCredentialProfile::S3 {
        access_key_id: "AKID".into(),
        secret_access_key: "secret-key".into(),
        session_token: None,
        sync_passphrase: Some("wrong-passphrase".into()),
    };
    let mut bundle = StoredCredentialBundle::new(profile.clone());
    bundle
        .stage_transition(PendingCredentialProfile {
            credentials: profile,
            operation_id: "rotation-fresh-freeze".into(),
            target_vault_id: settings.cloud_sync.vault_id.clone(),
            target_generation_id: "generation-next".into(),
            phase: CredentialTransitionPhase::Prepared,
        })
        .unwrap();
    save_credential_bundle(&credentials, &settings.cloud_sync.remote_id, &bundle)
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, &credentials)
        .await
        .unwrap();
    let active = VaultDocument::active(
        VaultIdentity {
            format_version: 2,
            vault_id: settings.cloud_sync.vault_id.clone(),
            generation_id: settings.cloud_sync.generation_id.clone(),
        },
        VaultProtection::encrypted(&settings.cloud_sync.vault_id, "correct-passphrase").unwrap(),
    );
    load_or_create_vault(backend.as_ref(), active.clone())
        .await
        .unwrap();
    begin_generation_freeze_owned(
        backend.as_ref(),
        &active,
        "generation-next",
        VaultProtection::plain(),
        "rotation-fresh-freeze",
        "device-restart",
        1,
        2,
    )
    .await
    .unwrap();
    let vault_path = RemotePath::parse("v1/vault.json").unwrap();
    let before = backend.get(&vault_path).await.unwrap();

    let error = service
        .reconcile_pending_credential_transition(settings)
        .await
        .unwrap_err();

    assert!(matches!(error, AppError::Crypto(_)), "{error:?}");
    let after = backend.get(&vault_path).await.unwrap();
    assert_eq!(after.etag, before.etag);
    assert_eq!(after.bytes, before.bytes);
}

#[tokio::test]
async fn fresh_pending_publishing_with_a_wrong_active_passphrase_does_not_touch_remote_state() {
    let (mut service, _data_dir) = service_with_local_session_fixture().await;
    let credentials = MemoryCredentialStore::default();
    service.credentials = Arc::new(credentials.clone());
    let server = TestS3::start("AKID", None).await;
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        encryption_enabled: true,
        remote_id: "remote-fresh-publishing-wrong-passphrase".into(),
        vault_id: "vault-fresh-publishing-wrong-passphrase".into(),
        generation_id: "generation-old".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "fresh-publishing-wrong-passphrase".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    let profile = StoredCloudCredentialProfile::S3 {
        access_key_id: "AKID".into(),
        secret_access_key: "secret-key".into(),
        session_token: None,
        sync_passphrase: Some("wrong-passphrase".into()),
    };
    let mut bundle = StoredCredentialBundle::new(profile.clone());
    bundle
        .stage_transition(PendingCredentialProfile {
            credentials: profile,
            operation_id: "rotation-fresh-publishing".into(),
            target_vault_id: settings.cloud_sync.vault_id.clone(),
            target_generation_id: "generation-next".into(),
            phase: CredentialTransitionPhase::Prepared,
        })
        .unwrap();
    save_credential_bundle(&credentials, &settings.cloud_sync.remote_id, &bundle)
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, &credentials)
        .await
        .unwrap();
    let active = VaultDocument::active(
        VaultIdentity {
            format_version: 2,
            vault_id: settings.cloud_sync.vault_id.clone(),
            generation_id: settings.cloud_sync.generation_id.clone(),
        },
        VaultProtection::encrypted(&settings.cloud_sync.vault_id, "correct-passphrase").unwrap(),
    );
    load_or_create_vault(backend.as_ref(), active.clone())
        .await
        .unwrap();
    let current = load_versioned_identity(backend.as_ref()).await.unwrap();
    let head_path = format!(
        "v1/generations/{}/devices/device-other/head.json",
        settings.cloud_sync.generation_id
    );
    let replacement_head = HeadDocument {
        generation_id: settings.cloud_sync.generation_id.clone(),
        device_id: "device-other".into(),
        end_seq: 1,
        path: format!(
            "v1/generations/{}/devices/device-other/bundles/1-1-test.acmb",
            settings.cloud_sync.generation_id
        ),
        sha256: "test".into(),
    };
    begin_head_publish(
        backend.as_ref(),
        &current,
        HeadPublishRequest {
            operation_id: "expired-publishing".into(),
            owner_device_id: "device-other".into(),
            started_at_ms: 1,
            lease_expires_at_ms: 2,
            head_path: head_path.clone(),
            expected_head_etag: None,
            replacement_head_json: serde_json::to_string(&replacement_head).unwrap(),
            published_mutation_count: 1,
        },
    )
    .await
    .unwrap();
    let vault_before = vault_object_snapshot(backend.as_ref()).await;
    let head_path = RemotePath::parse(&head_path).unwrap();
    let head_before = backend.get(&head_path).await.ok();

    let error = service
        .reconcile_pending_credential_transition(settings)
        .await
        .unwrap_err();

    assert!(matches!(error, AppError::Crypto(_)), "{error:?}");
    assert_eq!(vault_object_snapshot(backend.as_ref()).await, vault_before);
    assert_eq!(backend.get(&head_path).await.ok(), head_before);
}

#[tokio::test]
async fn sync_recovers_an_abandoned_frozen_vault_before_publishing() {
    let service = service_with_local_session().await;
    let server = TestS3::start("AKID", None).await;
    let remote_id = format!("frozen-test-{}", uuid::Uuid::new_v4().simple());
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: remote_id.clone(),
        vault_id: "vault-frozen-test".into(),
        generation_id: "generation-old".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "service-frozen-recovery".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    service
        .credentials
        .set(
            &remote_id,
            SecretKind::S3AccessKeyId,
            SecretValue::new("AKID"),
        )
        .await
        .unwrap();
    service
        .credentials
        .set(
            &remote_id,
            SecretKind::S3SecretAccessKey,
            SecretValue::new("secret-key"),
        )
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, service.credentials.as_ref())
        .await
        .unwrap();
    let active = VaultDocument::active(
        VaultIdentity {
            format_version: 2,
            vault_id: settings.cloud_sync.vault_id.clone(),
            generation_id: settings.cloud_sync.generation_id.clone(),
        },
        VaultProtection::plain(),
    );
    load_or_create_vault(backend.as_ref(), active.clone())
        .await
        .unwrap();
    let frozen = begin_generation_freeze_owned(
        backend.as_ref(),
        &active,
        "generation-abandoned",
        VaultProtection::plain(),
        "operation-abandoned",
        "device-abandoned",
        1,
        2,
    )
    .await
    .unwrap();
    assert!(matches!(frozen.state, VaultState::Frozen { .. }));

    service.sync_once_locked(settings).await.unwrap();

    let recovered = load_versioned_identity(backend.as_ref()).await.unwrap();
    assert_eq!(recovered.state, VaultState::Active);
    assert_eq!(recovered.identity.generation_id, "generation-old");
    assert_eq!(
        service.sync_store.pending_mutation_count().await.unwrap(),
        0
    );
}
