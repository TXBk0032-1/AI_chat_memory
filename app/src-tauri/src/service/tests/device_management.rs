use super::support::*;
use crate::error::AppError;
use crate::sync::backend::{CloudError, CloudErrorKind, RemotePath};

#[tokio::test]
async fn remove_cloud_device_record_deletes_only_the_requested_remote_prefix() {
    let fx = CloudFixture::empty(&auto_prefix("service-remove")).await;
    fx.service
        .sync_store
        .initialize_device("device-local", "本机")
        .await
        .unwrap();
    let remote_head = RemotePath::parse(&format!(
        "v1/generations/{}/devices/device-remote/head.json",
        fx.settings.generation_id
    ))
    .unwrap();
    fx.backend
        .put_if_absent(&remote_head, b"fixture")
        .await
        .unwrap();

    fx.service
        .remove_cloud_device_record("device-remote".into())
        .await
        .unwrap();

    assert_eq!(
        fx.backend.get(&remote_head).await.unwrap_err().kind(),
        "not_found"
    );
    assert!(
        fx.service
            .remove_cloud_device_record("device-local".into())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn remove_cloud_device_record_refreshes_devices_without_faking_sync_success() {
    // 删除单个远端设备不是一次完整的云同步成功——不得刷新
    // last_success_at/清空错误状态；且必须枚举删除设备前缀下的全部对象。
    let fx = CloudFixture::empty(&auto_prefix("service-svc7")).await;
    fx.service
        .sync_store
        .initialize_device("device-local", "本机")
        .await
        .unwrap();
    let bundle_sha = "ab".repeat(32);
    let remote_head = RemotePath::parse(&format!(
        "v1/generations/{}/devices/device-remote/head.json",
        fx.settings.generation_id
    ))
    .unwrap();
    let remote_bundle = RemotePath::parse(&format!(
        "v1/generations/{}/devices/device-remote/bundles/1-1-{bundle_sha}.acmb",
        fx.settings.generation_id
    ))
    .unwrap();
    fx.backend
        .put_if_absent(&remote_head, b"fixture")
        .await
        .unwrap();
    fx.backend
        .put_immutable(&remote_bundle, b"stale-bundle")
        .await
        .unwrap();
    fx.service
        .sync_store
        .set_remote_cursor(
            &fx.settings.generation_id,
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
    fx.service
        .mark_cloud_error(&AppError::Cloud(CloudError::new(
            CloudErrorKind::Protocol,
            "previous sync failure",
        )))
        .await;

    let status = fx
        .service
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
        fx.backend.get(&remote_head).await.unwrap_err().kind(),
        "not_found"
    );
    assert_eq!(
        fx.backend.get(&remote_bundle).await.unwrap_err().kind(),
        "not_found",
        "the whole device prefix must be enumerated and deleted, not just the head"
    );
    assert!(
        fx.service
            .sync_store
            .remote_cursor(&fx.settings.generation_id, "device-remote")
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
    let fx = CloudFixture::empty(&auto_prefix("service-svc7-fail")).await;
    fx.service
        .sync_store
        .initialize_device("device-local", "本机")
        .await
        .unwrap();
    sqlx::query("DROP TABLE sync_remote_cursors")
        .execute(&fx.service.pool)
        .await
        .unwrap();

    let error = fx
        .service
        .remove_cloud_device_record("device-remote".into())
        .await
        .expect_err("cursor cleanup failure must surface instead of faking success");

    assert!(
        matches!(error, AppError::InvalidData(ref message) if message.contains("游标清理失败")),
        "got {error:?}"
    );
}
