use super::support::*;
use crate::error::AppError;
use crate::models::{
    CloudBackendKind, CloudCredentialInput, CloudSyncSettings, S3CloudSyncSettings,
};
use crate::sync::credentials::{
    CredentialStore, MemoryCredentialStore, SecretKind, SecretValue, StoredCloudCredentialProfile,
    load_credential_bundle, load_or_migrate_credential_bundle,
};
use crate::sync::test_s3_server::TestS3;
use std::sync::Arc;

#[tokio::test]
async fn s3_credential_update_rolls_back_an_atomic_bundle_write_failure() {
    const OLD: [&str; 4] = ["OLD-AKID", "old-secret", "old-token", "old-passphrase"];
    for delete_optional in [false, true] {
        let mut service = service_with_local_session().await;
        let new_token = (!delete_optional).then_some("new-token");
        let server = TestS3::start("NEW-AKID", new_token).await;
        let credentials = FaultInjectingCredentialStore::default();
        for (kind, value) in [
            SecretKind::S3AccessKeyId,
            SecretKind::S3SecretAccessKey,
            SecretKind::S3SessionToken,
            SecretKind::SyncPassphrase,
        ]
        .into_iter()
        .zip(OLD)
        {
            credentials
                .inner
                .set("remote-atomic", kind, SecretValue::new(value))
                .await
                .unwrap();
        }
        service.credentials = Arc::new(credentials.clone());
        let mut active = service.settings().await;
        active.cloud_sync = CloudSyncSettings {
            backend: CloudBackendKind::S3,
            connection_verified: true,
            remote_id: "remote-atomic".into(),
            s3: S3CloudSyncSettings {
                endpoint_url: server.endpoint().into(),
                region: "us-east-1".into(),
                bucket: "archive".into(),
                prefix: format!("credential-failure-{delete_optional}"),
                force_path_style: true,
            },
            ..CloudSyncSettings::default()
        };
        service.settings.update(active.clone()).await.unwrap();
        load_or_migrate_credential_bundle(&credentials, &active.cloud_sync)
            .await
            .unwrap()
            .unwrap();
        service.ensure_local_device().await.unwrap();
        service.sync_store.seed_local_baseline().await.unwrap();
        let settings_before = service.settings().await;
        let outbox_before = service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap();
        credentials.arm(1);
        let replacement = CloudCredentialInput::S3 {
            access_key_id: "NEW-AKID".into(),
            secret_access_key: "secret-key".into(),
            session_token: new_token.map(str::to_owned),
            sync_password: (!delete_optional).then(|| "new-passphrase".into()),
        };
        let mut next = settings_before.clone();
        next.setup_complete = !next.setup_complete;

        let error = service
            .update_settings_with_cloud_credentials(next, Some(replacement))
            .await
            .unwrap_err();

        assert!(matches!(error, AppError::Credential(_)));
        assert_eq!(
            serde_json::to_value(service.settings().await).unwrap(),
            serde_json::to_value(settings_before).unwrap(),
            "atomic credential failure changed settings"
        );
        assert_eq!(
            service
                .sync_store
                .pending_mutations(i64::MAX)
                .await
                .unwrap(),
            outbox_before,
            "atomic credential failure changed the outbox"
        );
        let bundle = load_credential_bundle(&credentials, "remote-atomic")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            bundle.active,
            StoredCloudCredentialProfile::S3 {
                access_key_id: OLD[0].into(),
                secret_access_key: OLD[1].into(),
                session_token: Some(OLD[2].into()),
                sync_passphrase: Some(OLD[3].into()),
            }
        );
        assert!(bundle.pending.is_none());
    }
}

#[tokio::test]
async fn settings_write_failure_restores_every_s3_credential_and_the_active_draft() {
    let (mut service, data_dir) = service_with_local_session_fixture().await;
    let server = TestS3::start("NEW-AKID", Some("new-token")).await;
    let credentials = MemoryCredentialStore::default();
    for (kind, value) in [
        (SecretKind::S3AccessKeyId, "OLD-AKID"),
        (SecretKind::S3SecretAccessKey, "old-secret"),
        (SecretKind::S3SessionToken, "old-token"),
        (SecretKind::SyncPassphrase, "old-passphrase"),
    ] {
        credentials
            .set("remote-settings-failure", kind, SecretValue::new(value))
            .await
            .unwrap();
    }
    service.credentials = Arc::new(credentials);
    let mut active = service.settings().await;
    active.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        connection_verified: true,
        remote_id: "remote-settings-failure".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "settings-write-rollback".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(active).await.unwrap();
    service.ensure_local_device().await.unwrap();
    service.sync_store.seed_local_baseline().await.unwrap();
    let settings_before = service.settings().await;
    let outbox_before = service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();
    // 让 settings.json 的原子写入必然失败：persist 已改用 uuid 临时名，
    // 固定的 *.tmp 注入点不再生效，改为把主文件路径替换成目录，
    // 使最终的 rename(tmp→main) 必然报 Io 错误。
    tokio::fs::remove_file(data_dir.join("settings.json"))
        .await
        .unwrap();
    tokio::fs::create_dir(data_dir.join("settings.json"))
        .await
        .unwrap();
    let mut next = settings_before.clone();
    next.setup_complete = !next.setup_complete;
    let error = service
        .update_settings_with_cloud_credentials(
            next,
            Some(CloudCredentialInput::S3 {
                access_key_id: "NEW-AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: Some("new-token".into()),
                sync_password: Some("new-passphrase".into()),
            }),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, AppError::Io(_)), "{error:?}");
    assert_eq!(
        serde_json::to_value(service.settings().await).unwrap(),
        serde_json::to_value(settings_before).unwrap()
    );
    assert_eq!(
        service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        outbox_before
    );
    let bundle = load_credential_bundle(service.credentials.as_ref(), "remote-settings-failure")
        .await
        .unwrap()
        .expect("legacy credentials should have been migrated before the failed update");
    assert_eq!(
        bundle.active,
        StoredCloudCredentialProfile::S3 {
            access_key_id: "OLD-AKID".into(),
            secret_access_key: "old-secret".into(),
            session_token: Some("old-token".into()),
            sync_passphrase: Some("old-passphrase".into()),
        }
    );
    assert!(bundle.pending.is_none());
}
