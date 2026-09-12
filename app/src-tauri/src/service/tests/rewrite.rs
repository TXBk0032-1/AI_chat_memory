use super::support::*;
use crate::error::AppError;
use crate::models::{CloudBackendKind, CloudSyncSettings, S3CloudSyncSettings};
use crate::sync::backend::RemotePath;
use crate::sync::credentials::{SecretKind, SecretValue};
use crate::sync::factory::backend_from_store;
use crate::sync::test_s3_server::TestS3;
use crate::sync::vault::{
    VaultDocument, VaultIdentity, VaultProtection, load_or_create_identity, load_or_create_vault,
    load_versioned_identity,
};

#[tokio::test]
async fn rewrite_rejects_remote_plain_when_local_encryption_is_persisted() {
    let (service, settings_before, backend, _server) =
        configured_s3_service_for_sync_guard_tests("rewrite-plain-fence", true, "old-passphrase")
            .await;
    load_or_create_vault(
        backend.as_ref(),
        VaultDocument::active(
            VaultIdentity {
                format_version: 2,
                vault_id: settings_before.cloud_sync.vault_id.clone(),
                generation_id: settings_before.cloud_sync.generation_id.clone(),
            },
            VaultProtection::plain(),
        ),
    )
    .await
    .unwrap();
    let vault_before = vault_object_snapshot(backend.as_ref()).await;
    let device = service.ensure_local_device().await.unwrap();
    let head_path = RemotePath::parse(&format!(
        "v1/generations/{}/devices/{}/head.json",
        settings_before.cloud_sync.generation_id, device.device_id
    ))
    .unwrap();
    let head_before = backend.get(&head_path).await.ok();
    let outbox_before = service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();

    let error = service.rewrite_cloud_archive().await.unwrap_err();

    assert!(matches!(error, AppError::InvalidData(_)), "{error:?}");
    assert_eq!(
        serde_json::to_value(service.settings().await).unwrap(),
        serde_json::to_value(settings_before).unwrap()
    );
    assert_eq!(vault_object_snapshot(backend.as_ref()).await, vault_before);
    assert_eq!(backend.get(&head_path).await.ok(), head_before);
    assert_eq!(
        service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        outbox_before
    );
}

#[tokio::test]
async fn rewrite_cloud_archive_cleans_partial_generation_when_baseline_publish_fails() {
    let service = service_with_local_session().await;
    let server = TestS3::start("AKID", None).await;
    let remote_id = format!("pre-cas-test-{}", uuid::Uuid::new_v4().simple());
    let old_generation = "generation-old".to_owned();
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: remote_id.clone(),
        vault_id: "vault-pre-cas-test".into(),
        generation_id: old_generation.clone(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "service-pre-cas".into(),
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
    let expected = VaultIdentity {
        format_version: 2,
        vault_id: settings.cloud_sync.vault_id.clone(),
        generation_id: old_generation.clone(),
    };
    load_or_create_identity(backend.as_ref(), expected.clone())
        .await
        .unwrap();
    let old_object =
        crate::sync::backend::RemotePath::parse("v1/generations/generation-old/keep.bin").unwrap();
    backend.put_if_absent(&old_object, b"keep").await.unwrap();
    service.ensure_local_device().await.unwrap();
    service.sync_store.seed_local_baseline().await.unwrap();
    let pending_before = service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();
    server.fail_baseline_get_after(0).await;

    let error = service.rewrite_cloud_archive().await.unwrap_err();

    assert!(matches!(error, crate::error::AppError::Cloud(_)));
    assert_eq!(
        service.settings().await.cloud_sync.generation_id,
        old_generation
    );
    assert_eq!(
        crate::sync::vault::load_versioned_identity(backend.as_ref())
            .await
            .unwrap()
            .identity,
        expected
    );
    assert_eq!(
        service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        pending_before
    );
    assert_eq!(backend.get(&old_object).await.unwrap().bytes, b"keep");
    let generations = backend
        .list_depth_one(&crate::sync::backend::RemotePath::parse("v1/generations").unwrap())
        .await
        .unwrap();
    assert_eq!(
        generations
            .into_iter()
            .filter(|entry| entry.is_collection)
            .map(|entry| entry.name)
            .collect::<Vec<_>>(),
        vec!["generation-old"]
    );
}

#[tokio::test]
async fn rewrite_cloud_archive_keeps_activated_generation_when_followup_sync_fails() {
    let service = service_with_local_session().await;
    let server = TestS3::start("AKID", None).await;
    let remote_id = format!("rollback-test-{}", uuid::Uuid::new_v4().simple());
    let old_generation = "generation-old".to_owned();
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: remote_id.clone(),
        vault_id: "vault-rollback-test".into(),
        generation_id: old_generation.clone(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "service-rollback".into(),
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
    let expected = VaultIdentity {
        format_version: 2,
        vault_id: settings.cloud_sync.vault_id.clone(),
        generation_id: old_generation.clone(),
    };
    load_or_create_identity(backend.as_ref(), expected.clone())
        .await
        .unwrap();
    let old_object =
        crate::sync::backend::RemotePath::parse("v1/generations/generation-old/keep.bin").unwrap();
    backend.put_if_absent(&old_object, b"keep").await.unwrap();
    service.ensure_local_device().await.unwrap();
    service.sync_store.seed_local_baseline().await.unwrap();
    let pending_before = service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();
    server.fail_baseline_get_after(2).await;

    let error = service.rewrite_cloud_archive().await.unwrap_err();

    assert!(matches!(error, crate::error::AppError::Cloud(_)));
    let persisted_generation = service.settings().await.cloud_sync.generation_id;
    assert_ne!(persisted_generation, old_generation);
    let remote_identity = crate::sync::vault::load_versioned_identity(backend.as_ref())
        .await
        .unwrap()
        .identity;
    assert_eq!(remote_identity.vault_id, expected.vault_id);
    assert_eq!(remote_identity.generation_id, persisted_generation);
    assert_eq!(
        service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        pending_before
    );
    assert_eq!(backend.get(&old_object).await.unwrap().bytes, b"keep");
    let generations = backend
        .list_depth_one(&crate::sync::backend::RemotePath::parse("v1/generations").unwrap())
        .await
        .unwrap();
    let generation_names = generations
        .into_iter()
        .filter(|entry| entry.is_collection)
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    assert!(generation_names.contains(&"generation-old".to_owned()));
    assert!(generation_names.contains(&persisted_generation));
}

#[tokio::test]
async fn rewrite_cloud_archive_switches_the_persisted_and_remote_generation() {
    let service = service_with_local_session().await;
    let server = TestS3::start("AKID", None).await;
    let remote_id = format!("rewrite-test-{}", uuid::Uuid::new_v4().simple());
    let old_generation = "generation-old".to_owned();
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: remote_id.clone(),
        vault_id: "vault-rewrite-test".into(),
        generation_id: old_generation.clone(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "service-rewrite".into(),
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
    load_or_create_identity(
        backend.as_ref(),
        VaultIdentity {
            format_version: 2,
            vault_id: settings.cloud_sync.vault_id.clone(),
            generation_id: old_generation.clone(),
        },
    )
    .await
    .unwrap();

    let result = service.rewrite_cloud_archive().await;
    let updated = service.settings().await;
    let remote = load_or_create_identity(
        backend.as_ref(),
        VaultIdentity {
            format_version: 2,
            vault_id: "ignored".into(),
            generation_id: "ignored".into(),
        },
    )
    .await
    .unwrap();
    service
        .credentials
        .delete(&remote_id, SecretKind::S3AccessKeyId)
        .await
        .unwrap();
    service
        .credentials
        .delete(&remote_id, SecretKind::S3SecretAccessKey)
        .await
        .unwrap();

    result.unwrap();
    assert_ne!(updated.cloud_sync.generation_id, old_generation);
    assert_eq!(remote.vault_id, updated.cloud_sync.vault_id);
    assert_eq!(remote.generation_id, updated.cloud_sync.generation_id);
}

#[tokio::test]
async fn rewrite_cloud_archive_surfaces_and_marks_a_failed_local_generation_commit() {
    // 云端已提交新代次后本地 settings 写入失败，必须重试并在彻底失败时
    // 显式报告错配，让下一次同步以远端代次自愈，而不是静默留下代次错配窗口。
    let (service, data_dir) = service_with_local_session_fixture().await;
    let server = TestS3::start("AKID", None).await;
    let remote_id = format!("svc6-test-{}", uuid::Uuid::new_v4().simple());
    let old_generation = "generation-old".to_owned();
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: remote_id.clone(),
        vault_id: "vault-svc6-test".into(),
        generation_id: old_generation.clone(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "service-svc6".into(),
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
    load_or_create_identity(
        backend.as_ref(),
        VaultIdentity {
            format_version: 2,
            vault_id: settings.cloud_sync.vault_id.clone(),
            generation_id: old_generation.clone(),
        },
    )
    .await
    .unwrap();
    service.ensure_local_device().await.unwrap();
    service.sync_store.seed_local_baseline().await.unwrap();
    // 让 settings.json 的原子写入必然失败（uuid 临时名无法预占，
    // 改为把主文件路径替换成目录，使最终 rename 必然失败）。
    tokio::fs::remove_file(data_dir.join("settings.json"))
        .await
        .unwrap();
    tokio::fs::create_dir(data_dir.join("settings.json"))
        .await
        .unwrap();

    let error = service.rewrite_cloud_archive().await.unwrap_err();

    assert!(
        matches!(error, AppError::Configuration(ref message)
            if message.contains("云端存档已提交新代次") && message.contains("本地设置写入失败")),
        "the failed local commit must surface the compensation context, got {error:?}"
    );
    assert_eq!(
        service.settings().await.cloud_sync.generation_id,
        old_generation,
        "the persisted settings must still hold the stale generation"
    );
    let remote_generation = load_versioned_identity(backend.as_ref())
        .await
        .unwrap()
        .identity
        .generation_id;
    assert_ne!(
        remote_generation, old_generation,
        "the remote generation must have advanced"
    );

    // 补偿语义：解除写入故障后，下一次同步以远端代次自愈本地设置。
    tokio::fs::remove_dir(data_dir.join("settings.json"))
        .await
        .unwrap();
    service
        .sync_once_locked(service.settings().await)
        .await
        .unwrap();
    assert_eq!(
        service.settings().await.cloud_sync.generation_id,
        remote_generation
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}
