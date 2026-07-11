use serde_json::Value;
use sqlx::SqlitePool;
use std::{io::Read, sync::Arc};

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
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
        let mut file = archive
            .by_name("conversations.json")
            .map_err(|_| AppError::InvalidData("ZIP 中缺少 conversations.json".into()))?;
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
        let sessions = database::search(&self.pool, &query).await?;
        let total = sessions.len();
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
}
