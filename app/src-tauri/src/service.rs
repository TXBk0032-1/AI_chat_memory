use serde_json::Value;
use sqlx::SqlitePool;
use std::{io::Read, path::Path, sync::Arc};
use tokio::sync::RwLock;

use crate::{
    database,
    error::{AppError, Result},
    models::*,
    normalizer,
    settings::SettingsStore,
};

#[derive(Clone)]
pub struct AppService {
    pub pool: SqlitePool,
    pub settings: Arc<SettingsStore>,
    pub api_status: Arc<RwLock<ApiStatus>>,
}

impl AppService {
    pub async fn import(&self, request: ImportRequest) -> Result<ImportResponse> {
        let normalized = request
            .sessions
            .iter()
            .map(|raw| normalizer::normalize_session(&request.platform, raw))
            .collect::<Result<Vec<_>>>()?;
        Ok(ImportResponse {
            imported: database::import_sessions(&self.pool, &normalized).await?,
            skipped: 0,
        })
    }
    pub async fn import_deepseek_zip(&self, bytes: Vec<u8>) -> Result<ImportResponse> {
        if bytes.len() > 128 * 1024 * 1024 {
            return Err(AppError::InvalidData("ZIP 文件超过 128 MB 限制".into()));
        }
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
        let mut file = archive
            .by_name("conversations.json")
            .map_err(|_| AppError::InvalidData("ZIP 中缺少 conversations.json".into()))?;
        if file.size() > 512 * 1024 * 1024 {
            return Err(AppError::InvalidData(
                "conversations.json 解压后超过 512 MB 限制".into(),
            ));
        }
        if file.compressed_size() > 0 && file.size() / file.compressed_size() > 200 {
            return Err(AppError::InvalidData("ZIP 压缩比异常".into()));
        }
        let mut content = String::new();
        file.read_to_string(&mut content)?;
        let conversations: Vec<Value> = serde_json::from_str(&content)?;
        let normalized = conversations
            .iter()
            .map(normalizer::normalize_deepseek_export)
            .collect::<Result<Vec<_>>>()?;
        Ok(ImportResponse {
            imported: database::import_sessions(&self.pool, &normalized).await?,
            skipped: 0,
        })
    }
    pub async fn list(&self, query: SearchQuery) -> Result<SessionList> {
        let total = database::count(&self.pool, &query).await? as usize;
        let sessions = database::search(&self.pool, &query).await?;
        Ok(SessionList { sessions, total })
    }
    pub async fn detail(&self, id: &str) -> Result<SessionDetail> {
        database::get_session(&self.pool, id).await
    }
    pub async fn delete(&self, id: &str) -> Result<()> {
        database::delete_session(&self.pool, id).await
    }
    pub async fn sync_status(&self, platform: &str) -> Result<Option<String>> {
        database::sync_status(&self.pool, platform).await
    }
    pub async fn migrate_legacy(&self, path: &Path) -> Result<()> {
        database::migrate_from_legacy(&self.pool, path).await?;
        let mut settings = self.settings.get().await;
        settings.migrated_legacy_database = true;
        self.settings.update(settings).await?;
        Ok(())
    }
    pub async fn api_status(&self) -> ApiStatus {
        self.api_status.read().await.clone()
    }
    pub async fn set_api_status(&self, status: ApiStatus) {
        *self.api_status.write().await = status;
    }
}
