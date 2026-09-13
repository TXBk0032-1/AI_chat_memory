use super::support::*;
use crate::error::AppError;
use crate::sync::backend::RemotePath;
use crate::sync::credentials::{CredentialStore, SecretKind};
use crate::sync::vault::load_versioned_identity;

#[tokio::test]
async fn rewrite_rejects_remote_plain_when_local_encryption_is_persisted() {
    let fx = CloudFixture::plain(&auto_prefix("rewrite-plain-fence"))
        .await
        .with_encryption_enabled(true)
        .await
        .with_local_passphrase("old-passphrase")
        .await;
    let vault_before = vault_object_snapshot(fx.backend.as_ref()).await;
    let device = fx.service.ensure_local_device().await.unwrap();
    let head_path = RemotePath::parse(&format!(
        "v1/generations/{}/devices/{}/head.json",
        fx.settings.generation_id, device.device_id
    ))
    .unwrap();
    let head_before = fx.backend.get(&head_path).await.ok();
    let outbox_before = fx
        .service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();

    let error = fx.service.rewrite_cloud_archive().await.unwrap_err();

    assert!(matches!(error, AppError::InvalidData(_)), "{error:?}");
    fx.assert_cloud_config_unchanged().await;
    fx.assert_remote_untouched(&vault_before).await;
    assert_eq!(fx.backend.get(&head_path).await.ok(), head_before);
    assert_eq!(
        fx.service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        outbox_before
    );
}

#[tokio::test]
async fn rewrite_cloud_archive_cleans_partial_generation_when_baseline_publish_fails() {
    let fx = CloudFixture::plain(&auto_prefix("service-pre-cas")).await;
    let old_object = RemotePath::parse("v1/generations/generation-old/keep.bin").unwrap();
    fx.backend
        .put_if_absent(&old_object, b"keep")
        .await
        .unwrap();
    fx.service.ensure_local_device().await.unwrap();
    fx.service.sync_store.seed_local_baseline().await.unwrap();
    let pending_before = fx
        .service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();
    fx.server.fail_baseline_get_after(0).await;

    let error = fx.service.rewrite_cloud_archive().await.unwrap_err();

    assert!(matches!(error, AppError::Cloud(_)));
    assert_eq!(
        fx.service.settings().await.cloud_sync.generation_id,
        "generation-old"
    );
    assert_eq!(
        load_versioned_identity(fx.backend.as_ref())
            .await
            .unwrap()
            .identity
            .generation_id,
        "generation-old"
    );
    assert_eq!(
        fx.service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        pending_before
    );
    assert_eq!(fx.backend.get(&old_object).await.unwrap().bytes, b"keep");
    fx.assert_generation_layout(&["generation-old"]).await;
}

#[tokio::test]
async fn rewrite_cloud_archive_keeps_activated_generation_when_followup_sync_fails() {
    let fx = CloudFixture::plain(&auto_prefix("service-rollback")).await;
    let old_object = RemotePath::parse("v1/generations/generation-old/keep.bin").unwrap();
    fx.backend
        .put_if_absent(&old_object, b"keep")
        .await
        .unwrap();
    fx.service.ensure_local_device().await.unwrap();
    fx.service.sync_store.seed_local_baseline().await.unwrap();
    let pending_before = fx
        .service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();
    fx.server.fail_baseline_get_after(2).await;

    let error = fx.service.rewrite_cloud_archive().await.unwrap_err();

    assert!(matches!(error, AppError::Cloud(_)));
    let persisted_generation = fx.service.settings().await.cloud_sync.generation_id;
    assert_ne!(persisted_generation, "generation-old");
    let remote_identity = load_versioned_identity(fx.backend.as_ref())
        .await
        .unwrap()
        .identity;
    assert_eq!(remote_identity.vault_id, fx.settings.vault_id);
    assert_eq!(remote_identity.generation_id, persisted_generation);
    assert_eq!(
        fx.service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        pending_before
    );
    assert_eq!(fx.backend.get(&old_object).await.unwrap().bytes, b"keep");
    let generations = fx
        .backend
        .list_depth_one(&RemotePath::parse("v1/generations").unwrap())
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
    let fx = CloudFixture::plain(&auto_prefix("service-rewrite")).await;

    let result = fx.service.rewrite_cloud_archive().await;
    let updated = fx.service.settings().await;
    let remote = load_versioned_identity(fx.backend.as_ref())
        .await
        .unwrap()
        .identity;
    fx.credentials
        .delete(&fx.settings.remote_id, SecretKind::S3AccessKeyId)
        .await
        .unwrap();
    fx.credentials
        .delete(&fx.settings.remote_id, SecretKind::S3SecretAccessKey)
        .await
        .unwrap();

    result.unwrap();
    assert_ne!(updated.cloud_sync.generation_id, "generation-old");
    assert_eq!(remote.vault_id, updated.cloud_sync.vault_id);
    assert_eq!(remote.generation_id, updated.cloud_sync.generation_id);
}

#[tokio::test]
async fn rewrite_cloud_archive_surfaces_and_marks_a_failed_local_generation_commit() {
    // 云端已提交新代次后本地 settings 写入失败，必须重试并在彻底失败时
    // 显式报告错配，让下一次同步以远端代次自愈，而不是静默留下代次错配窗口。
    let fx = CloudFixture::plain(&auto_prefix("service-svc6")).await;
    fx.service.ensure_local_device().await.unwrap();
    fx.service.sync_store.seed_local_baseline().await.unwrap();
    // 让 settings.json 的原子写入必然失败（uuid 临时名无法预占，
    // 改为把主文件路径替换成目录，使最终 rename 必然失败）。
    tokio::fs::remove_file(fx.data_dir().join("settings.json"))
        .await
        .unwrap();
    tokio::fs::create_dir(fx.data_dir().join("settings.json"))
        .await
        .unwrap();

    let error = fx.service.rewrite_cloud_archive().await.unwrap_err();

    assert!(
        matches!(error, AppError::Configuration(ref message)
            if message.contains("云端存档已提交新代次") && message.contains("本地设置写入失败")),
        "the failed local commit must surface the compensation context, got {error:?}"
    );
    assert_eq!(
        fx.service.settings().await.cloud_sync.generation_id,
        "generation-old",
        "the persisted settings must still hold the stale generation"
    );
    let remote_generation = load_versioned_identity(fx.backend.as_ref())
        .await
        .unwrap()
        .identity
        .generation_id;
    assert_ne!(
        remote_generation, "generation-old",
        "the remote generation must have advanced"
    );

    // 补偿语义：解除写入故障后，下一次同步以远端代次自愈本地设置。
    tokio::fs::remove_dir(fx.data_dir().join("settings.json"))
        .await
        .unwrap();
    fx.service
        .sync_once_locked(fx.service.settings().await)
        .await
        .unwrap();
    assert_eq!(
        fx.service.settings().await.cloud_sync.generation_id,
        remote_generation
    );
}
