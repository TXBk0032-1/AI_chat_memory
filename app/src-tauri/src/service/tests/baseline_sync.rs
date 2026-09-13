use super::super::{CloudSyncCommand, CloudSyncScheduler};
use super::support::*;
use crate::models::{
    CloudBackendKind, CloudCredentialInput, CloudSyncSettings, S3CloudSyncSettings,
};
use crate::sync::backend::RemotePath;
use crate::sync::test_s3_server::TestS3;
use crate::sync::types::SyncTrigger;
use crate::sync::vault::{VaultIdentity, replace_identity};
use tokio::sync::mpsc;

#[tokio::test]
async fn enabling_cloud_sync_queues_the_seeded_baseline_for_automatic_sync() {
    let (mut service, _data_dir) = service_with_local_session_fixture().await;
    let (sender, mut receiver) = mpsc::channel(1);
    service.cloud_sync_scheduler = CloudSyncScheduler { sender };
    let server = TestS3::start("AKID", None).await;
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "automatic-baseline".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };

    service
        .update_settings_with_cloud_credentials(
            settings,
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: None,
                sync_password: None,
            }),
        )
        .await
        .unwrap();

    assert!(matches!(
        receiver.try_recv(),
        Ok(CloudSyncCommand::Trigger(SyncTrigger::LocalMutation))
    ));
}

#[tokio::test]
async fn sync_adopts_a_remote_generation_and_replays_the_local_baseline_once() {
    let fx = CloudFixture::plain(&auto_prefix("service-adoption")).await;
    let old_generation = fx.settings.generation_id.clone();
    let old_identity = VaultIdentity {
        format_version: 2,
        vault_id: fx.settings.vault_id.clone(),
        generation_id: old_generation.clone(),
    };

    fx.service
        .sync_once_locked(fx.service.settings().await)
        .await
        .unwrap();
    let device = fx.service.ensure_local_device().await.unwrap();
    let old_head = RemotePath::parse(&format!(
        "v1/generations/{old_generation}/devices/{}/head.json",
        device.device_id
    ))
    .unwrap();
    assert!(fx.backend.get(&old_head).await.is_ok());
    let version_before: (i64, i64, String) = sqlx::query_as(
        "SELECT version_wall_ms, version_counter, version_device_id
         FROM sync_entity_versions WHERE platform = 'fixture' AND platform_session_id = 'local-1'",
    )
    .fetch_one(&fx.service.pool)
    .await
    .unwrap();
    let new_identity = VaultIdentity {
        generation_id: "generation-new".into(),
        ..old_identity.clone()
    };
    replace_identity(fx.backend.as_ref(), &old_identity, new_identity.clone())
        .await
        .unwrap();

    fx.service
        .sync_once_locked(fx.service.settings().await)
        .await
        .unwrap();

    assert_eq!(
        fx.service.settings().await.cloud_sync.generation_id,
        new_identity.generation_id
    );
    let publication: (String, String) = sqlx::query_as(
        "SELECT vault_id, generation_id FROM sync_publication_state WHERE singleton = 1",
    )
    .fetch_one(&fx.service.pool)
    .await
    .unwrap();
    assert_eq!(
        publication,
        (
            new_identity.vault_id.clone(),
            new_identity.generation_id.clone()
        )
    );
    assert_eq!(
        sqlx::query_as::<_, (i64, i64, String)>(
            "SELECT version_wall_ms, version_counter, version_device_id
             FROM sync_entity_versions WHERE platform = 'fixture' AND platform_session_id = 'local-1'",
        )
        .fetch_one(&fx.service.pool)
        .await
        .unwrap(),
        version_before
    );
    assert_eq!(
        fx.service
            .sync_store
            .pending_mutation_count()
            .await
            .unwrap(),
        0
    );
    assert!(fx.backend.get(&old_head).await.is_ok());
    let new_head = RemotePath::parse(&format!(
        "v1/generations/{}/devices/{}/head.json",
        new_identity.generation_id, device.device_id
    ))
    .unwrap();
    assert!(fx.backend.get(&new_head).await.is_ok());
    let next_seq_after_adoption: i64 =
        sqlx::query_scalar("SELECT next_seq FROM sync_device_state WHERE singleton = 1")
            .fetch_one(&fx.service.pool)
            .await
            .unwrap();

    fx.service
        .sync_once_locked(fx.service.settings().await)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT next_seq FROM sync_device_state WHERE singleton = 1")
            .fetch_one(&fx.service.pool)
            .await
            .unwrap(),
        next_seq_after_adoption
    );
}
