use super::support::*;
use crate::error::AppError;
use crate::models::CloudCredentialInput;
use crate::sync::backend::RemotePath;
use crate::sync::credentials::{
    CredentialStore, CredentialTransitionPhase, MemoryCredentialStore, SecretKind, SecretValue,
    load_credential_bundle,
};
use crate::sync::engine::HeadDocument;
use crate::sync::vault::load_versioned_identity;
use std::sync::Arc;

#[tokio::test]
async fn sync_password_change_reads_old_chain_and_commits_new_encrypted_generation() {
    let credentials: Arc<dyn CredentialStore> = Arc::new(MemoryCredentialStore::default());
    let (service, _server, settings_before, backend, _data_dir) =
        configured_encrypted_s3_service(credentials).await;
    let old_protection = load_versioned_identity(backend.as_ref())
        .await
        .unwrap()
        .protection;
    let outbox_before = service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();

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

    let bundle = crate::sync::credentials::load_credential_bundle(
        service.credentials.as_ref(),
        &updated.cloud_sync.remote_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(bundle.active.sync_passphrase(), Some("new-passphrase"));
    assert!(bundle.pending.is_none());
    service
        .sync_once_locked(updated.clone())
        .await
        .expect("the next process run must use the committed credential profile");

    assert_ne!(
        updated.cloud_sync.generation_id,
        settings_before.cloud_sync.generation_id
    );
    assert_eq!(
        load_versioned_identity(backend.as_ref())
            .await
            .unwrap()
            .identity
            .generation_id,
        updated.cloud_sync.generation_id
    );
    assert_eq!(
        service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        outbox_before
    );
    let head_path = RemotePath::parse(&format!(
        "v1/generations/{}/devices/baseline/head.json",
        updated.cloud_sync.generation_id
    ))
    .unwrap();
    let head: HeadDocument =
        serde_json::from_slice(&backend.get(&head_path).await.unwrap().bytes).unwrap();
    let bundle = backend
        .get(&RemotePath::parse(&head.path).unwrap())
        .await
        .unwrap();
    let new_protector = load_versioned_identity(backend.as_ref())
        .await
        .unwrap()
        .protection
        .derive_protector(&updated.cloud_sync.vault_id, "new-passphrase")
        .unwrap()
        .unwrap();
    crate::sync::bundle::open_bundle_protected(
        &bundle.bytes,
        &crate::sync::bundle::BundleLimits::default(),
        Some(new_protector.as_ref()),
    )
    .unwrap();
    let old_protector = old_protection
        .derive_protector(&updated.cloud_sync.vault_id, "old-passphrase")
        .unwrap()
        .unwrap();
    assert!(
        crate::sync::bundle::open_bundle_protected(
            &bundle.bytes,
            &crate::sync::bundle::BundleLimits::default(),
            Some(old_protector.as_ref()),
        )
        .is_err()
    );
}

#[tokio::test]
async fn enabling_encryption_reads_the_old_plain_chain_and_commits_an_encrypted_generation() {
    let fx = CloudFixture::plain(&auto_prefix("enable-encryption")).await;
    let settings_before = fx.service.settings().await;

    let mut next = settings_before.clone();
    next.cloud_sync.encryption_enabled = true;
    let updated = fx
        .service
        .update_settings_with_cloud_credentials(
            next,
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: None,
                sync_password: Some("new-passphrase".into()),
            }),
        )
        .await
        .unwrap();

    assert!(updated.cloud_sync.encryption_enabled);
    assert_ne!(
        updated.cloud_sync.generation_id,
        settings_before.cloud_sync.generation_id
    );
    let remote = load_versioned_identity(fx.backend.as_ref()).await.unwrap();
    assert_eq!(
        remote.identity.generation_id,
        updated.cloud_sync.generation_id
    );
    assert_eq!(
        remote.protection.algorithm,
        crate::sync::bundle::ProtectionAlgorithm::XChaCha20Poly1305
    );
}

#[tokio::test]
async fn disabling_encryption_reads_the_old_chain_and_commits_a_plain_generation() {
    let credentials: Arc<dyn CredentialStore> = Arc::new(MemoryCredentialStore::default());
    let (service, _server, settings_before, backend, _data_dir) =
        configured_encrypted_s3_service(credentials).await;
    let mut next = settings_before.clone();
    next.cloud_sync.encryption_enabled = false;

    let updated = service
        .update_settings_with_cloud_credentials(
            next,
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: Some("old-token".into()),
                sync_password: None,
            }),
        )
        .await
        .unwrap();

    assert!(!updated.cloud_sync.encryption_enabled);
    assert_ne!(
        updated.cloud_sync.generation_id,
        settings_before.cloud_sync.generation_id
    );
    let stored =
        load_credential_bundle(service.credentials.as_ref(), &updated.cloud_sync.remote_id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(stored.active.sync_passphrase(), None);
    assert!(stored.pending.is_none());
    assert_eq!(
        load_versioned_identity(backend.as_ref())
            .await
            .unwrap()
            .identity
            .generation_id,
        updated.cloud_sync.generation_id
    );
    let head_path = RemotePath::parse(&format!(
        "v1/generations/{}/devices/baseline/head.json",
        updated.cloud_sync.generation_id
    ))
    .unwrap();
    let head: HeadDocument =
        serde_json::from_slice(&backend.get(&head_path).await.unwrap().bytes).unwrap();
    let bundle = backend
        .get(&RemotePath::parse(&head.path).unwrap())
        .await
        .unwrap();
    let decoded = crate::sync::bundle::open_bundle(
        &bundle.bytes,
        &crate::sync::bundle::BundleLimits::default(),
    )
    .unwrap();
    assert_eq!(
        decoded.header.protection,
        crate::sync::bundle::ProtectionAlgorithm::Plain
    );
}

#[tokio::test]
async fn credential_failure_before_rotation_leaves_the_remote_generation_untouched() {
    let injecting = FaultInjectingCredentialStore::default();
    let credentials: Arc<dyn CredentialStore> = Arc::new(injecting.clone());
    let (service, _server, settings_before, backend, _data_dir) =
        configured_encrypted_s3_service(credentials).await;
    let outbox_before = service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();
    injecting.arm(1);

    let error = service
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
        .unwrap_err();

    assert!(matches!(error, AppError::Credential(_)), "{error:?}");
    assert_eq!(
        serde_json::to_value(service.settings().await).unwrap(),
        serde_json::to_value(settings_before.clone()).unwrap()
    );
    assert_eq!(
        service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        outbox_before
    );
    let bundle = load_credential_bundle(
        service.credentials.as_ref(),
        &settings_before.cloud_sync.remote_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(bundle.active.sync_passphrase(), Some("old-passphrase"));
    assert!(bundle.pending.is_none());
    assert_eq!(
        load_versioned_identity(backend.as_ref())
            .await
            .unwrap()
            .identity
            .generation_id,
        settings_before.cloud_sync.generation_id
    );
    let generations = backend
        .list_depth_one(&RemotePath::parse("v1/generations").unwrap())
        .await
        .unwrap();
    assert_eq!(
        generations
            .into_iter()
            .filter(|entry| entry.is_collection)
            .map(|entry| entry.name)
            .collect::<Vec<_>>(),
        vec![settings_before.cloud_sync.generation_id]
    );
}

#[tokio::test]
async fn settings_write_failure_after_rotation_keeps_the_active_generation_and_new_credentials() {
    let credentials: Arc<dyn CredentialStore> = Arc::new(MemoryCredentialStore::default());
    let (service, _server, settings_before, backend, data_dir) =
        configured_encrypted_s3_service(credentials).await;
    // uuid 临时名无法预占，改为把主文件路径替换成目录注入写失败。
    tokio::fs::remove_file(data_dir.join("settings.json"))
        .await
        .unwrap();
    tokio::fs::create_dir(data_dir.join("settings.json"))
        .await
        .unwrap();

    let error = service
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
        .unwrap_err();

    assert!(matches!(error, AppError::Io(_)), "{error:?}");
    assert_eq!(
        service.settings().await.cloud_sync.generation_id,
        settings_before.cloud_sync.generation_id
    );
    let bundle = load_credential_bundle(
        service.credentials.as_ref(),
        &settings_before.cloud_sync.remote_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(bundle.active.sync_passphrase(), Some("old-passphrase"));
    let pending = bundle
        .pending
        .as_ref()
        .expect("a committed remote rotation must remain recoverable");
    assert_eq!(
        pending.credentials.sync_passphrase(),
        Some("new-passphrase")
    );
    assert_eq!(pending.phase, CredentialTransitionPhase::RemoteCommitted);
    let remote = load_versioned_identity(backend.as_ref()).await.unwrap();
    assert_ne!(
        remote.identity.generation_id,
        settings_before.cloud_sync.generation_id
    );
    assert!(
        remote
            .protection
            .passphrase_matches(&remote.identity.vault_id, "new-passphrase")
            .unwrap()
    );
    assert!(
        !remote
            .protection
            .passphrase_matches(&remote.identity.vault_id, "old-passphrase")
            .unwrap()
    );
    let generations = backend
        .list_depth_one(&RemotePath::parse("v1/generations").unwrap())
        .await
        .unwrap()
        .into_iter()
        .filter(|entry| entry.is_collection)
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    assert!(generations.contains(&settings_before.cloud_sync.generation_id));
    assert!(generations.contains(&remote.identity.generation_id));
}

#[tokio::test]
async fn matching_remote_passphrase_corrects_the_credential_without_rotating_generation() {
    let credentials = MemoryCredentialStore::default();
    let credentials_arc: Arc<dyn CredentialStore> = Arc::new(credentials.clone());
    let (service, _server, settings_before, backend, _data_dir) =
        configured_encrypted_s3_service(credentials_arc).await;
    credentials
        .set(
            &settings_before.cloud_sync.remote_id,
            SecretKind::SyncPassphrase,
            SecretValue::new("incorrect-local-copy"),
        )
        .await
        .unwrap();

    let updated = service
        .update_settings_with_cloud_credentials(
            settings_before.clone(),
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: Some("old-token".into()),
                sync_password: Some("old-passphrase".into()),
            }),
        )
        .await
        .unwrap();

    assert_eq!(
        updated.cloud_sync.generation_id,
        settings_before.cloud_sync.generation_id
    );
    assert_eq!(
        load_versioned_identity(backend.as_ref())
            .await
            .unwrap()
            .identity
            .generation_id,
        settings_before.cloud_sync.generation_id
    );
    let bundle = load_credential_bundle(&credentials, &settings_before.cloud_sync.remote_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bundle.active.sync_passphrase(), Some("old-passphrase"));
    assert!(bundle.pending.is_none());
    let generations = backend
        .list_depth_one(&RemotePath::parse("v1/generations").unwrap())
        .await
        .unwrap()
        .into_iter()
        .filter(|entry| entry.is_collection)
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    assert_eq!(generations, vec![settings_before.cloud_sync.generation_id]);
}
