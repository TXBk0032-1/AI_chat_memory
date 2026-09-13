use super::super::{VaultPassphrase, VaultVerification};
use super::support::*;
use crate::error::AppError;
use crate::models::{
    AppSettings, CloudBackendKind, CloudCredentialInput, CloudSyncSettings, S3CloudSyncSettings,
};
use crate::sync::backend::RemotePath;
use crate::sync::credentials::{SecretKind, SecretValue};
use crate::sync::engine::HeadDocument;
use crate::sync::factory::backend_from_input;
use crate::sync::test_s3_server::TestS3;
use crate::sync::vault::{
    HeadPublishRequest, VaultDocument, VaultIdentity, VaultProtection, begin_head_publish,
    load_or_create_vault, load_versioned_identity,
};

#[tokio::test]
async fn sync_rejects_remote_plain_when_local_encryption_is_persisted() {
    let fx = CloudFixture::plain(&auto_prefix("plain-fence"))
        .await
        .with_encryption_enabled(true)
        .await
        .with_local_passphrase("local-passphrase")
        .await;
    let vault_path = RemotePath::parse("v1/vault.json").unwrap();
    let before = fx.backend.get(&vault_path).await.unwrap();

    let error = fx
        .service
        .sync_once_locked(fx.service.settings().await)
        .await
        .unwrap_err();

    assert!(
        matches!(error, AppError::InvalidData(_) | AppError::Crypto(_)),
        "{error:?}"
    );
    fx.assert_cloud_config_unchanged().await;
    let after = fx.backend.get(&vault_path).await.unwrap();
    assert_eq!(after.etag, before.etag);
    assert_eq!(after.bytes, before.bytes);
    assert!(
        fx.service
            .sync_store
            .device_state()
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn sync_wrong_passphrase_does_not_recover_expired_frozen_vault() {
    let fx = CloudFixture::encrypted(
        &auto_prefix("wrong-passphrase-frozen"),
        "correct-passphrase",
    )
    .await
    .frozen_by_other_device("expired-freeze", LeaseLifecycle::Expired)
    .await
    .with_local_passphrase("wrong-passphrase")
    .await;
    let vault_path = RemotePath::parse("v1/vault.json").unwrap();
    let before = fx.backend.get(&vault_path).await.unwrap();

    let error = fx
        .service
        .sync_once_locked(fx.service.settings().await)
        .await
        .unwrap_err();

    assert!(matches!(error, AppError::Crypto(_)), "{error:?}");
    fx.assert_cloud_config_unchanged().await;
    let after = fx.backend.get(&vault_path).await.unwrap();
    assert_eq!(after.etag, before.etag);
    assert_eq!(after.bytes, before.bytes);
}

#[tokio::test]
async fn sync_wrong_passphrase_does_not_recover_publishing_vault() {
    let fx = CloudFixture::encrypted(
        &auto_prefix("wrong-passphrase-publishing"),
        "correct-passphrase",
    )
    .await
    .publishing_by_other_device(LeaseLifecycle::Expired)
    .await
    .with_local_passphrase("wrong-passphrase")
    .await;
    let vault_path = RemotePath::parse("v1/vault.json").unwrap();
    let before = fx.backend.get(&vault_path).await.unwrap();
    let head_path = RemotePath::parse(&format!(
        "v1/generations/{}/devices/device-other/head.json",
        fx.settings.generation_id
    ))
    .unwrap();
    let before_head = fx.backend.get(&head_path).await.ok();

    let error = fx
        .service
        .sync_once_locked(fx.service.settings().await)
        .await
        .unwrap_err();

    assert!(matches!(error, AppError::Crypto(_)), "{error:?}");
    fx.assert_cloud_config_unchanged().await;
    let after = fx.backend.get(&vault_path).await.unwrap();
    assert_eq!(after.etag, before.etag);
    assert_eq!(after.bytes, before.bytes);
    assert_eq!(fx.backend.get(&head_path).await.ok(), before_head);
}

#[tokio::test]
async fn sync_rejects_plain_active_from_a_frozen_recovery_reread_without_mutation() {
    let fx = CloudFixture::encrypted(&auto_prefix("frozen-reread-plain"), "correct-passphrase")
        .await
        .frozen_by_other_device("expired-frozen-reread-plain", LeaseLifecycle::Expired)
        .await;
    let device = fx.service.ensure_local_device().await.unwrap();
    fx.service.sync_store.seed_local_baseline().await.unwrap();
    let head_path = RemotePath::parse(&format!(
        "v1/generations/{}/devices/{}/head.json",
        fx.settings.generation_id, device.device_id
    ))
    .unwrap();
    let vault_before = vault_object_snapshot(fx.backend.as_ref()).await;
    let head_before = fx.backend.get(&head_path).await.ok();
    let outbox_before = fx
        .service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();
    let device_before = fx.service.sync_store.device_state().await.unwrap();
    let scripted = VaultDocument::active(
        VaultIdentity {
            format_version: 2,
            vault_id: fx.settings.vault_id.clone(),
            generation_id: fx.settings.generation_id.clone(),
        },
        VaultProtection::plain(),
    );
    fx.server
        .script_vault_change_after_gets(1, serde_json::to_vec(&scripted).unwrap())
        .await;

    let error = fx.service.sync_now().await.unwrap_err();

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
    assert_eq!(
        fx.service.sync_store.device_state().await.unwrap(),
        device_before
    );
}

#[tokio::test]
async fn verified_vault_rejects_changed_identity_from_a_frozen_recovery_reread() {
    let fx = CloudFixture::encrypted(&auto_prefix("frozen-reread-identity"), "correct-passphrase")
        .await
        .frozen_by_other_device("expired-frozen-reread-identity", LeaseLifecycle::Expired)
        .await;
    let vault_before = vault_object_snapshot(fx.backend.as_ref()).await;
    let changed_vault_id = "vault-changed-during-recovery";
    let scripted = VaultDocument::active(
        VaultIdentity {
            format_version: 2,
            vault_id: changed_vault_id.into(),
            generation_id: fx.settings.generation_id.clone(),
        },
        VaultProtection::encrypted(changed_vault_id, "correct-passphrase").unwrap(),
    );
    fx.server
        .script_vault_change_after_gets(1, serde_json::to_vec(&scripted).unwrap())
        .await;

    let result = fx
        .service
        .load_verified_vault(
            fx.backend.as_ref(),
            &fx.settings,
            VaultVerification {
                create_if_missing: false,
                expected_vault_id: Some(&fx.settings.vault_id),
                fence_encryption_enabled: true,
                expected_algorithm: None,
                proposed: None,
                passphrase: VaultPassphrase::Stored,
            },
        )
        .await;
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("changed vault identity was accepted after frozen recovery"),
    };

    assert!(matches!(error, AppError::InvalidData(_)), "{error:?}");
    fx.assert_remote_untouched(&vault_before).await;
}

#[tokio::test]
async fn saving_settings_rejects_remote_plain_when_local_encryption_is_persisted() {
    let fx = CloudFixture::plain(&auto_prefix("settings-plain-fence"))
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
    let mut next = fx.service.settings().await;
    next.setup_complete = !next.setup_complete;

    let error = fx
        .service
        .update_settings_with_cloud_credentials(
            next,
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: None,
                sync_password: Some("old-passphrase".into()),
            }),
        )
        .await
        .unwrap_err();

    assert!(
        matches!(error, AppError::InvalidData(_) | AppError::Crypto(_)),
        "{error:?}"
    );
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
async fn joining_existing_vault_with_wrong_passphrase_does_not_recover_expired_frozen_vault() {
    let (fx, draft) =
        CloudFixture::join_candidate(&auto_prefix("join-wrong-frozen"), "correct-passphrase").await;
    let fx = fx
        .frozen_by_other_device("join-expired-freeze", LeaseLifecycle::Expired)
        .await;
    let vault_before = vault_object_snapshot(fx.backend.as_ref()).await;
    let settings_before = fx.service.settings().await;
    let outbox_before = fx
        .service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();

    let error = fx
        .service
        .update_settings_with_cloud_credentials(
            AppSettings {
                cloud_sync: draft,
                ..settings_before.clone()
            },
            Some(wrong_join_credentials()),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, AppError::Crypto(_)), "{error:?}");
    assert_eq!(
        serde_json::to_value(fx.service.settings().await).unwrap(),
        serde_json::to_value(settings_before).unwrap()
    );
    fx.assert_remote_untouched(&vault_before).await;
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
async fn joining_existing_vault_with_wrong_passphrase_does_not_recover_publishing_vault() {
    let (fx, draft) =
        CloudFixture::join_candidate(&auto_prefix("join-wrong-publishing"), "correct-passphrase")
            .await;
    let current = load_versioned_identity(fx.backend.as_ref()).await.unwrap();
    let head_path = format!(
        "v1/generations/{}/devices/device-other/head.json",
        current.identity.generation_id
    );
    let replacement_head = HeadDocument {
        generation_id: current.identity.generation_id.clone(),
        device_id: "device-other".into(),
        end_seq: 1,
        path: format!(
            "v1/generations/{}/devices/device-other/bundles/1-1-test.acmb",
            current.identity.generation_id
        ),
        sha256: "test".into(),
    };
    begin_head_publish(
        fx.backend.as_ref(),
        &current,
        HeadPublishRequest {
            operation_id: "join-expired-publish".into(),
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
    let vault_before = vault_object_snapshot(fx.backend.as_ref()).await;
    let head_path = RemotePath::parse(&head_path).unwrap();
    let head_before = fx.backend.get(&head_path).await.ok();
    let settings_before = fx.service.settings().await;
    let outbox_before = fx
        .service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();

    let error = fx
        .service
        .update_settings_with_cloud_credentials(
            AppSettings {
                cloud_sync: draft,
                ..settings_before.clone()
            },
            Some(wrong_join_credentials()),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, AppError::Crypto(_)), "{error:?}");
    assert_eq!(
        serde_json::to_value(fx.service.settings().await).unwrap(),
        serde_json::to_value(settings_before).unwrap()
    );
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
async fn connection_test_prepares_a_draft_without_persisting_local_or_remote_state() {
    let (service, _data_dir) = service_with_local_session_fixture().await;
    service.ensure_local_device().await.unwrap();
    service.sync_store.seed_local_baseline().await.unwrap();
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
    let server = TestS3::start("AKID", Some("session-token")).await;
    let draft = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: false,
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "draft-only".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    let credentials = CloudCredentialInput::S3 {
        access_key_id: "AKID".into(),
        secret_access_key: "secret-key".into(),
        session_token: Some("session-token".into()),
        sync_password: None,
    };

    let result = service
        .test_cloud_sync_connection(draft.clone(), credentials.clone())
        .await
        .unwrap();

    assert!(result.ok);
    assert!(result.cloud_sync.connection_verified);
    assert_eq!(result.cloud_sync.backend, CloudBackendKind::S3);
    assert_eq!(
        serde_json::to_value(service.settings().await).unwrap(),
        serde_json::to_value(settings_before).unwrap(),
        "connection testing changed active settings"
    );
    assert_eq!(
        service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        outbox_before,
        "connection testing changed the live outbox"
    );
    assert_eq!(
        service
            .credentials
            .get("default", SecretKind::WebDavPassword)
            .await
            .unwrap()
            .unwrap()
            .expose_secret(),
        "old-webdav-password"
    );
    assert!(
        service
            .credentials
            .get(&result.cloud_sync.remote_id, SecretKind::S3AccessKeyId)
            .await
            .unwrap()
            .is_none(),
        "connection testing persisted draft credentials"
    );
    let backend = backend_from_input(&result.cloud_sync, &credentials).unwrap();
    assert_eq!(
        backend
            .get(&RemotePath::parse("v1/vault.json").unwrap())
            .await
            .unwrap_err()
            .kind(),
        "not_found",
        "connection testing created the active vault identity"
    );
}

#[tokio::test]
async fn joining_an_existing_vault_rejects_plain_and_encrypted_policy_mismatches() {
    for remote_encrypted in [false, true] {
        let (service, _data_dir) = service_with_local_session_fixture().await;
        let server = TestS3::start("AKID", None).await;
        let requested_encryption = !remote_encrypted;
        let cloud_sync = CloudSyncSettings {
            backend: CloudBackendKind::S3,
            enabled: true,
            encryption_enabled: requested_encryption,
            s3: S3CloudSyncSettings {
                endpoint_url: server.endpoint().into(),
                region: "us-east-1".into(),
                bucket: "archive".into(),
                prefix: format!("join-policy-mismatch-{remote_encrypted}"),
                force_path_style: true,
            },
            ..CloudSyncSettings::default()
        };
        let credentials = CloudCredentialInput::S3 {
            access_key_id: "AKID".into(),
            secret_access_key: "secret-key".into(),
            session_token: None,
            sync_password: requested_encryption.then(|| "candidate-passphrase".into()),
        };
        let backend = backend_from_input(&cloud_sync, &credentials).unwrap();
        let remote_protection = if remote_encrypted {
            crate::test_support::test_protection("remote-vault", "remote-passphrase")
        } else {
            VaultProtection::plain()
        };
        load_or_create_vault(
            backend.as_ref(),
            VaultDocument::active(
                VaultIdentity {
                    format_version: 2,
                    vault_id: "remote-vault".into(),
                    generation_id: "remote-generation".into(),
                },
                remote_protection.clone(),
            ),
        )
        .await
        .unwrap();
        let tested = service
            .test_cloud_sync_connection(cloud_sync, credentials.clone())
            .await
            .unwrap();
        let remote_id = tested.cloud_sync.remote_id.clone();
        let settings_before = service.settings().await;

        let error = service
            .update_settings_with_cloud_credentials(
                AppSettings {
                    cloud_sync: tested.cloud_sync,
                    ..settings_before.clone()
                },
                Some(credentials),
            )
            .await
            .unwrap_err();

        assert!(matches!(error, AppError::InvalidData(_)), "{error:?}");
        assert_eq!(
            serde_json::to_value(service.settings().await).unwrap(),
            serde_json::to_value(settings_before).unwrap()
        );
        assert_eq!(
            load_versioned_identity(backend.as_ref())
                .await
                .unwrap()
                .protection,
            remote_protection
        );
        for kind in [
            SecretKind::S3AccessKeyId,
            SecretKind::S3SecretAccessKey,
            SecretKind::S3SessionToken,
            SecretKind::SyncPassphrase,
        ] {
            assert!(
                service
                    .credentials
                    .get(&remote_id, kind)
                    .await
                    .unwrap()
                    .is_none(),
                "mismatched join persisted {kind:?}"
            );
        }
    }
}
