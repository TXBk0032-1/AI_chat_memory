use serde_json::Value;
use sqlx::SqlitePool;
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::RwLock;

use crate::{
    database,
    embedding::EmbeddingManager,
    error::{AppError, Result},
    models::*,
    normalizer,
    semantic::SemanticEngine,
    settings::SettingsStore,
};

#[derive(Clone)]
pub struct AppService {
    pool: SqlitePool,
    settings: Arc<SettingsStore>,
    semantic: Arc<SemanticEngine>,
    api_status: Arc<RwLock<ApiStatus>>,
    last_userscript_request_at: Arc<RwLock<Option<u64>>>,
}

impl AppService {
    pub async fn new(
        pool: SqlitePool,
        settings: Arc<SettingsStore>,
        data_dir: PathBuf,
    ) -> Result<Self> {
        let settings_value = settings.get().await;
        let embeddings =
            EmbeddingManager::from_settings(data_dir.clone(), settings_value.semantic_search)
                .await?;
        let semantic = Arc::new(SemanticEngine::new(pool.clone(), data_dir, embeddings));
        semantic.start_worker();
        Ok(Self {
            pool,
            settings,
            semantic,
            api_status: Arc::new(RwLock::new(ApiStatus::Starting)),
            last_userscript_request_at: Arc::new(RwLock::new(None)),
        })
    }

    pub async fn settings(&self) -> AppSettings {
        self.settings.get().await
    }

    pub async fn update_settings(&self, settings: AppSettings) -> Result<AppSettings> {
        let previous = self.settings.get().await;
        let updated = self.settings.update(settings).await?;
        if previous.semantic_search != updated.semantic_search {
            self.semantic
                .reload_embeddings(updated.semantic_search.clone())
                .await?;
        }
        Ok(updated)
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
        for session in &normalized {
            if let Ok(Some(id)) = sqlx::query_scalar::<_, String>(
                "SELECT id FROM sessions WHERE platform = ? AND platform_session_id = ?",
            )
            .bind(&session.platform)
            .bind(&session.platform_session_id)
            .fetch_optional(&self.pool)
            .await
            {
                let _ = self.semantic.request_session_index(&id).await;
            }
        }
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
        for session in &normalized {
            if let Ok(Some(id)) = sqlx::query_scalar::<_, String>(
                "SELECT id FROM sessions WHERE platform = ? AND platform_session_id = ?",
            )
            .bind(&session.platform)
            .bind(&session.platform_session_id)
            .fetch_optional(&self.pool)
            .await
            {
                let _ = self.semantic.request_session_index(&id).await;
            }
        }
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
        self.semantic.search_sessions(query).await
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
        mode: Option<SearchMode>,
    ) -> Result<Vec<SessionSearchHit>> {
        let settings = self.settings().await;
        let mode = mode.unwrap_or(settings.semantic_search.default_mode);
        self.semantic.search_session_hits(id, query, mode).await
    }

    pub async fn session_branches(&self, id: &str) -> Result<BranchOverview> {
        database::get_session_branches(&self.pool, id).await
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        database::delete_session(&self.pool, id).await?;
        let _ = self.semantic.delete_session(id).await;
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

    pub async fn semantic_status(&self) -> SemanticRuntimeStatus {
        self.semantic.runtime_status().await
    }

    pub async fn embedding_healthcheck(&self) -> EmbeddingHealth {
        self.semantic.healthcheck().await
    }

    pub async fn reindex_semantic(&self) -> Result<usize> {
        self.semantic.request_reindex_all().await
    }

    pub async fn download_local_model(
        &self,
        on_progress: Option<crate::embedding::local::DownloadProgressCallback>,
    ) -> Result<()> {
        self.semantic.ensure_local_model(on_progress).await
    }

    pub async fn import_local_model(&self, path: &Path) -> Result<()> {
        self.semantic.import_local_model(path).await
    }
}
