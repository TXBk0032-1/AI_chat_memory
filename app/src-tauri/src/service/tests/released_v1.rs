use super::support::*;
use crate::error::AppError;
use crate::models::CloudCredentialInput;
use crate::sync::backend::RemotePath;
use crate::sync::vault::{VaultProtection, load_versioned_identity};

#[tokio::test]
async fn released_v1_archive_bootstraps_plain_despite_stale_encryption_setting() {
    let fx = CloudFixture::released_v1(&auto_prefix("legacy-stale-encryption"), true).await;

    fx.service
        .sync_once_locked(fx.service.settings().await)
        .await
        .unwrap();

    let persisted = fx.service.settings().await;
    assert_eq!(persisted.cloud_sync.vault_id, "default");
    assert_eq!(persisted.cloud_sync.generation_id, "generation-1");
    assert!(!persisted.cloud_sync.encryption_enabled);
    let remote = load_versioned_identity(fx.backend.as_ref()).await.unwrap();
    assert_eq!(remote.protection, VaultProtection::plain());
    let vault_json: serde_json::Value = serde_json::from_slice(
        &fx.backend
            .get(&RemotePath::parse("v1/vault.json").unwrap())
            .await
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(
        vault_json
            .get("compatibility")
            .and_then(|value| value.as_str()),
        Some("released_v1_writers")
    );
    let imported_title: String = sqlx::query_scalar(
        "SELECT title FROM sessions
         WHERE platform = 'legacy' AND platform_session_id = 'remote-only'",
    )
    .fetch_one(&fx.service.pool)
    .await
    .unwrap();
    assert_eq!(imported_title, "released remote only");
}

#[tokio::test]
async fn released_v1_compatibility_fences_encryption_rotation() {
    let fx = CloudFixture::released_v1(&auto_prefix("legacy-encryption-fence"), false).await;
    fx.service
        .sync_once_locked(fx.service.settings().await)
        .await
        .unwrap();
    let before = fx.service.settings().await;
    let mut requested = before.clone();
    requested.cloud_sync.encryption_enabled = true;

    let error = fx
        .service
        .update_settings_with_cloud_credentials(
            requested,
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: None,
                sync_password: Some("replacement-passphrase".into()),
            }),
        )
        .await
        .unwrap_err();

    assert!(
        matches!(error, AppError::Configuration(ref message) if message.contains("重写云端存档")),
        "{error:?}"
    );
    assert_eq!(
        serde_json::to_value(fx.service.settings().await).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    let remote = load_versioned_identity(fx.backend.as_ref()).await.unwrap();
    assert_eq!(remote.identity.generation_id, "generation-1");
    assert_eq!(remote.protection, VaultProtection::plain());
}

#[tokio::test]
async fn rewrite_cloud_archive_explicitly_retires_released_v1_compatibility() {
    let fx = CloudFixture::released_v1(&auto_prefix("legacy-explicit-retirement"), false).await;
    fx.service
        .sync_once_locked(fx.service.settings().await)
        .await
        .unwrap();
    let before = fx.service.settings().await;

    fx.service.rewrite_cloud_archive().await.unwrap();

    let after = fx.service.settings().await;
    assert_ne!(
        after.cloud_sync.generation_id,
        before.cloud_sync.generation_id
    );
    let remote = load_versioned_identity(fx.backend.as_ref()).await.unwrap();
    assert_eq!(
        remote.identity.generation_id,
        after.cloud_sync.generation_id
    );
    let vault_json: serde_json::Value = serde_json::from_slice(
        &fx.backend
            .get(&RemotePath::parse("v1/vault.json").unwrap())
            .await
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert!(vault_json.get("compatibility").is_none());
    assert!(
        fx.backend
            .get(
                &RemotePath::parse(
                    "v1/generations/generation-1/devices/device-released/head.json",
                )
                .unwrap(),
            )
            .await
            .is_ok(),
        "explicit retirement must not delete released history"
    );
}
