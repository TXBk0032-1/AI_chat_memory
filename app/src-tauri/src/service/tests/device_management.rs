use super::support::*;
use crate::error::AppError;
use crate::models::{CloudBackendKind, CloudSyncSettings, S3CloudSyncSettings};
use crate::sync::backend::{CloudError, CloudErrorKind, RemotePath};
use crate::sync::credentials::{SecretKind, SecretValue};
use crate::sync::factory::backend_from_store;
use crate::sync::test_s3_server::TestS3;

#[tokio::test]
async fn remove_cloud_device_record_deletes_only_the_requested_remote_prefix() {
    let service = service_with_local_session().await;
    let server = TestS3::start("AKID", None).await;
    let remote_id = format!("remove-test-{}", uuid::Uuid::new_v4().simple());
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: remote_id.clone(),
        vault_id: "vault-remove-test".into(),
        generation_id: "generation-remove-test".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "service-remove".into(),
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
    service
        .sync_store
        .initialize_device("device-local", "本机")
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, service.credentials.as_ref())
        .await
        .unwrap();
    let remote_head = crate::sync::backend::RemotePath::parse(
        "v1/generations/generation-remove-test/devices/device-remote/head.json",
    )
    .unwrap();
    backend
        .put_if_absent(&remote_head, b"fixture")
        .await
        .unwrap();

    service
        .remove_cloud_device_record("device-remote".into())
        .await
        .unwrap();

    assert_eq!(
        backend.get(&remote_head).await.unwrap_err().kind(),
        "not_found"
    );
    assert!(
        service
            .remove_cloud_device_record("device-local".into())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn remove_cloud_device_record_refreshes_devices_without_faking_sync_success() {
    // 删除单个远端设备不是一次完整的云同步成功——不得刷新
    // last_success_at/清空错误状态；且必须枚举删除设备前缀下的全部对象。
    let service = service_with_local_session().await;
    let server = TestS3::start("AKID", None).await;
    let remote_id = format!("svc7-test-{}", uuid::Uuid::new_v4().simple());
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: remote_id.clone(),
        vault_id: "vault-svc7-test".into(),
        generation_id: "generation-svc7-test".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "service-svc7".into(),
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
    service
        .sync_store
        .initialize_device("device-local", "本机")
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, service.credentials.as_ref())
        .await
        .unwrap();
    let bundle_sha = "ab".repeat(32);
    let remote_head =
        RemotePath::parse("v1/generations/generation-svc7-test/devices/device-remote/head.json")
            .unwrap();
    let remote_bundle = RemotePath::parse(&format!(
        "v1/generations/generation-svc7-test/devices/device-remote/bundles/1-1-{bundle_sha}.acmb"
    ))
    .unwrap();
    backend
        .put_if_absent(&remote_head, b"fixture")
        .await
        .unwrap();
    backend
        .put_immutable(&remote_bundle, b"stale-bundle")
        .await
        .unwrap();
    service
        .sync_store
        .set_remote_cursor(
            "generation-svc7-test",
            "device-remote",
            &crate::sync::store::RemoteObjectAnchor {
                end_seq: 1,
                path: remote_bundle.display(),
                sha256: bundle_sha,
            },
            42,
        )
        .await
        .unwrap();
    // 预置一次失败的同步状态：删除设备记录不能把它洗成“成功”。
    service
        .mark_cloud_error(&AppError::Cloud(CloudError::new(
            CloudErrorKind::Protocol,
            "previous sync failure",
        )))
        .await;

    let status = service
        .remove_cloud_device_record("device-remote".into())
        .await
        .unwrap();

    assert_eq!(
        status.last_success_at, None,
        "removing a device record must not fabricate a sync success"
    );
    assert_eq!(
        status.last_error_code.as_deref(),
        Some("protocol"),
        "the previous sync error state must be preserved"
    );
    assert!(
        status
            .last_error_message
            .as_deref()
            .is_some_and(|message| message.contains("previous sync failure"))
    );
    assert!(
        !status
            .devices
            .iter()
            .any(|device| device.device_id == "device-remote"),
        "the deleted device must disappear from the refreshed device list"
    );
    assert_eq!(
        backend.get(&remote_head).await.unwrap_err().kind(),
        "not_found"
    );
    assert_eq!(
        backend.get(&remote_bundle).await.unwrap_err().kind(),
        "not_found",
        "the whole device prefix must be enumerated and deleted, not just the head"
    );
    assert!(
        service
            .sync_store
            .remote_cursor("generation-svc7-test", "device-remote")
            .await
            .unwrap()
            .is_none(),
        "the local sync cursor for the deleted device must be removed"
    );
}

#[tokio::test]
async fn remove_cloud_device_record_reports_cursor_cleanup_failures() {
    // 游标清理失败必须显式报错（远端已删、本地游标残留会导致下次
    // 同步以陈旧游标计算拉取起点），而不是静默成功。
    let service = service_with_local_session().await;
    let server = TestS3::start("AKID", None).await;
    let remote_id = format!("svc7-fail-test-{}", uuid::Uuid::new_v4().simple());
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: remote_id.clone(),
        vault_id: "vault-svc7-fail-test".into(),
        generation_id: "generation-svc7-fail-test".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "service-svc7-fail".into(),
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
    service
        .sync_store
        .initialize_device("device-local", "本机")
        .await
        .unwrap();
    sqlx::query("DROP TABLE sync_remote_cursors")
        .execute(&service.pool)
        .await
        .unwrap();

    let error = service
        .remove_cloud_device_record("device-remote".into())
        .await
        .expect_err("cursor cleanup failure must surface instead of faking success");

    assert!(
        matches!(error, AppError::InvalidData(ref message) if message.contains("游标清理失败")),
        "got {error:?}"
    );
}
