use serde_json::Value;
use sqlx::SqlitePool;
use std::{
    io::Read,
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
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
    pool: SqlitePool,
    settings: Arc<SettingsStore>,
    api_status: Arc<RwLock<ApiStatus>>,
    last_userscript_request_at: Arc<RwLock<Option<u64>>>,
}

impl AppService {
    pub fn new(pool: SqlitePool, settings: Arc<SettingsStore>) -> Self {
        Self {
            pool,
            settings,
            api_status: Arc::new(RwLock::new(ApiStatus::Starting)),
            last_userscript_request_at: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn settings(&self) -> AppSettings {
        self.settings.get().await
    }

    pub async fn update_settings(&self, settings: AppSettings) -> Result<AppSettings> {
        self.settings.update(settings).await
    }

    pub async fn rotate_secret(&self) -> Result<AppSettings> {
        self.settings.rotate_secret().await
    }

    pub async fn move_data_directory(&self, directory: &Path) -> Result<()> {
        tokio::fs::create_dir_all(directory).await?;
        let destination = directory.join("chat_memory.db");
        if destination.exists() {
            return Err(AppError::Configuration(
                "目标目录中已存在 chat_memory.db，请选择其他目录".into(),
            ));
        }
        sqlx::query("VACUUM INTO ?")
            .bind(destination.to_string_lossy().as_ref())
            .execute(&self.pool)
            .await?;
        let mut settings = self.settings().await;
        settings.data_directory = Some(directory.to_string_lossy().into_owned());
        self.update_settings(settings).await?;
        tracing::info!(destination=%directory.display(), "database copied to configured directory; restarting application");
        Ok(())
    }

    pub async fn set_close_behavior(&self, behavior: CloseBehavior) -> Result<()> {
        let mut settings = self.settings().await;
        settings.close_behavior = behavior;
        self.update_settings(settings).await?;
        Ok(())
    }
    pub async fn import(&self, request: ImportRequest) -> Result<ImportResponse> {
        let platform = request.platform.clone();
        let received = request.sessions.len();
        let normalized = request
            .sessions
            .iter()
            .map(|raw| normalizer::normalize_session(&request.platform, raw))
            .collect::<Result<Vec<_>>>()?;
        let imported = database::import_sessions(&self.pool, &normalized).await?;
        tracing::info!(%platform, received, imported, "session import completed");
        Ok(ImportResponse {
            imported,
            skipped: 0,
        })
    }
    pub async fn import_deepseek_zip(&self, bytes: Vec<u8>) -> Result<ImportResponse> {
        let archive_bytes = bytes.len();
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
        let imported = database::import_sessions(&self.pool, &normalized).await?;
        tracing::info!(
            archive_bytes,
            conversations = normalized.len(),
            imported,
            "DeepSeek archive import completed"
        );
        Ok(ImportResponse {
            imported,
            skipped: 0,
        })
    }
    pub async fn list(&self, query: SearchQuery) -> Result<SessionList> {
        let total = database::count(&self.pool, &query).await? as usize;
        let sessions = database::search(&self.pool, &query).await?;
        Ok(SessionList { sessions, total })
    }
    pub async fn open_session(&self, id: &str, anchor_seq: Option<i64>) -> Result<SessionOpen> {
        database::open_session(&self.pool, id, anchor_seq).await
    }
    pub async fn session_messages(
        &self,
        id: &str,
        start_seq: i64,
        limit: i64,
    ) -> Result<Vec<Message>> {
        database::get_session_messages(&self.pool, id, start_seq, limit).await
    }
    pub async fn session_search_hits(
        &self,
        id: &str,
        query: &str,
    ) -> Result<Vec<SessionSearchHit>> {
        database::search_session_hits(&self.pool, id, query).await
    }
    pub async fn session_branches(&self, id: &str) -> Result<BranchOverview> {
        database::get_session_branches(&self.pool, id).await
    }
    pub async fn delete(&self, id: &str) -> Result<()> {
        database::delete_session(&self.pool, id).await?;
        tracing::info!("session deleted");
        Ok(())
    }
    pub async fn sync_status(&self, platform: &str) -> Result<Option<String>> {
        database::sync_status(&self.pool, platform).await
    }
    pub async fn api_status(&self) -> ApiStatus {
        self.api_status.read().await.clone()
    }
    pub async fn set_api_status(&self, status: ApiStatus) {
        *self.api_status.write().await = status;
    }
    pub async fn mark_userscript_request(&self) {
        *self.last_userscript_request_at.write().await = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|value| value.as_secs());
    }
    pub async fn desktop_api_status(&self) -> DesktopApiStatus {
        let last = *self.last_userscript_request_at.read().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|value| value.as_secs());
        DesktopApiStatus {
            service: self.api_status().await,
            userscript_connected: last
                .zip(now)
                .is_some_and(|(last, now)| now.saturating_sub(last) <= 15),
            last_userscript_request_at: last,
        }
    }
}
