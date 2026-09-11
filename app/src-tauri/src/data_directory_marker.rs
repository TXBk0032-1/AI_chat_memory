use std::path::Path;

use tokio::io::AsyncWriteExt;

use crate::error::{AppError, Result};

/// Redirect marker 文件名。桌面端完成迁移后，在旧数据目录留下一个原子的、
/// 版本化的 JSON 标记；仍持有旧 SQLite 池的 MCP stdio 进程读取它后立即
/// 拒绝数据读取并提示重启。
#[allow(dead_code)] // wired by service/mcp migration follow-up batches
pub(crate) const DATA_DIRECTORY_REDIRECT_FILE: &str = ".ai-chat-memory-data-moved.json";

/// 当前 marker schema 版本。读取方只认这个版本，其他版本一律失败关闭。
#[allow(dead_code)] // wired by service/mcp migration follow-up batches
const REDIRECT_VERSION: u32 = 1;

/// 迁移标记 payload。`version` 用于未来的 schema 演进；`destination_hint`
/// 仅用于日志与用户提示，读取方绝不据此自动切换数据库池。
#[allow(dead_code)] // wired by service/mcp migration follow-up batches
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct DataDirectoryRedirect {
    pub version: u32,
    pub moved_at: String,
    pub destination_hint: String,
}

/// 在 `old_dir` 内原子发布迁移标记：先写 UUID 临时文件并 fsync，再 rename
/// 到固定 marker 名。任一环节失败都删除临时文件并返回错误，目录中永远不会
/// 出现半写的 marker；rename 落盘后即使掉电，也只会看到完整或缺失两种状态。
#[allow(dead_code)] // wired by service/mcp migration follow-up batches
pub(crate) async fn publish_redirect(old_dir: &Path, destination: &Path) -> Result<()> {
    let marker_path = old_dir.join(DATA_DIRECTORY_REDIRECT_FILE);
    // UUID 临时名与 SettingsStore 的 settings.json.tmp-* 同一惯例：并发
    // 发布者互不覆盖，崩溃残留不与正式 marker 混淆。
    let temporary = old_dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let payload = DataDirectoryRedirect {
        version: REDIRECT_VERSION,
        moved_at: chrono::Utc::now().to_rfc3339(),
        destination_hint: destination.display().to_string(),
    };
    let encoded = serde_json::to_vec(&payload)?;
    {
        let mut writer = tokio::io::BufWriter::new(tokio::fs::File::create(&temporary).await?);
        writer.write_all(&encoded).await?;
        writer.flush().await?;
        // FlushFileBuffers：rename 之后的掉电不得留下零字节 marker。
        writer.get_ref().sync_all().await?;
    }
    if let Err(error) = tokio::fs::rename(&temporary, &marker_path).await {
        // 正式 marker 未被触碰；删除 tmp 并向上传播错误，让迁移流程
        // 完整报告失败而不是留下半成品状态。
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error.into());
    }
    Ok(())
}

/// 读取 `data_dir` 中的迁移标记。不存在视为未迁移（`None`）；损坏的 JSON
/// 或未知版本一律失败关闭为 `AppError::InvalidData`——宁可拒绝读取，
/// 也不让持有旧池的进程在目录已被搬走后继续服务陈旧数据。
#[allow(dead_code)] // wired by service/mcp migration follow-up batches
pub(crate) async fn read_redirect(data_dir: &Path) -> Result<Option<DataDirectoryRedirect>> {
    let marker_path = data_dir.join(DATA_DIRECTORY_REDIRECT_FILE);
    let raw = match tokio::fs::read(&marker_path).await {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let redirect: DataDirectoryRedirect = serde_json::from_slice(&raw).map_err(|error| {
        tracing::error!(%error, path=%marker_path.display(), "data directory redirect marker is corrupt");
        AppError::InvalidData("数据目录迁移标记损坏，请重启 MCP 并检查数据目录".into())
    })?;
    if redirect.version != REDIRECT_VERSION {
        tracing::error!(
            version = redirect.version,
            path=%marker_path.display(),
            "data directory redirect marker has an unsupported version"
        );
        return Err(AppError::InvalidData(
            "数据目录迁移标记损坏，请重启 MCP 并检查数据目录".into(),
        ));
    }
    Ok(Some(redirect))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(label: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("acm-marker-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn tmp_files_in(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| {
                let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                name.ends_with(".tmp").then_some(name)
            })
            .collect()
    }

    #[tokio::test]
    async fn publish_redirect_writes_versioned_marker_without_tmp_residue() {
        let old_dir = scratch_dir("publish");
        let destination = old_dir
            .parent()
            .unwrap()
            .join(format!("acm-marker-dest-{}", uuid::Uuid::new_v4()));
        publish_redirect(&old_dir, &destination).await.unwrap();

        let redirect = read_redirect(&old_dir).await.unwrap().unwrap();
        assert_eq!(redirect.version, 1);
        assert_eq!(redirect.destination_hint, destination.display().to_string());
        assert!(!redirect.moved_at.is_empty());
        assert!(tmp_files_in(&old_dir).is_empty(), "发布后不得残留 tmp 文件");
        let _ = std::fs::remove_dir_all(old_dir);
    }

    #[tokio::test]
    async fn read_redirect_returns_invalid_data_for_corrupted_marker() {
        let data_dir = scratch_dir("corrupted");
        tokio::fs::write(data_dir.join(DATA_DIRECTORY_REDIRECT_FILE), "not-json{")
            .await
            .unwrap();

        let error = read_redirect(&data_dir).await.unwrap_err();
        assert!(
            matches!(error, AppError::InvalidData(_)),
            "损坏标记必须失败关闭，实际错误：{error:?}"
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn read_redirect_fails_closed_on_unsupported_version() {
        let data_dir = scratch_dir("version");
        tokio::fs::write(
            data_dir.join(DATA_DIRECTORY_REDIRECT_FILE),
            r#"{"version":99,"moved_at":"2026-09-11T00:00:00+00:00","destination_hint":"D:/elsewhere"}"#,
        )
        .await
        .unwrap();

        let error = read_redirect(&data_dir).await.unwrap_err();
        assert!(
            matches!(error, AppError::InvalidData(_)),
            "未知版本必须失败关闭，实际错误：{error:?}"
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn read_redirect_returns_none_when_marker_missing() {
        let data_dir = scratch_dir("missing");
        assert_eq!(read_redirect(&data_dir).await.unwrap(), None);
        let _ = std::fs::remove_dir_all(data_dir);
    }
}
