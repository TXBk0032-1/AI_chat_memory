use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, RwLock};

use super::index;
use crate::{
    embedding::EmbeddingManager,
    error::Result,
    models::{
        EmbeddingHealth, SearchMode, SearchQuery, SemanticRuntimeStatus, SemanticStatus,
        SessionList, SessionSearchHit,
    },
};

#[derive(Clone)]
pub struct SemanticEngine {
    pool: SqlitePool,
    data_dir: PathBuf,
    embeddings: Arc<RwLock<EmbeddingManager>>,
    wake: Arc<Notify>,
    worker_running: Arc<Mutex<bool>>,
    last_error: Arc<RwLock<Option<String>>>,
}

impl SemanticEngine {
    pub fn new(pool: SqlitePool, data_dir: PathBuf, embeddings: EmbeddingManager) -> Self {
        Self {
            pool,
            data_dir,
            embeddings: Arc::new(RwLock::new(embeddings)),
            wake: Arc::new(Notify::new()),
            worker_running: Arc::new(Mutex::new(false)),
            last_error: Arc::new(RwLock::new(None)),
        }
    }

    pub fn start_worker(self: &Arc<Self>) {
        let engine = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            engine.worker_loop().await;
        });
        self.wake.notify_one();
    }

    pub async fn reload_embeddings(
        &self,
        settings: crate::models::SemanticSearchSettings,
    ) -> Result<()> {
        let manager = EmbeddingManager::from_settings(self.data_dir.clone(), settings).await?;
        *self.embeddings.write().await = manager;
        self.request_reindex_all().await?;
        self.wake.notify_one();
        Ok(())
    }

    pub async fn request_session_index(&self, session_id: &str) -> Result<()> {
        let identity = self.embeddings.read().await.identity();
        index::queue_session_chunks(&self.pool, session_id, &identity).await?;
        self.wake.notify_one();
        Ok(())
    }

    pub async fn request_reindex_all(&self) -> Result<usize> {
        let identity = self.embeddings.read().await.identity();
        let queued = index::queue_all_sessions(&self.pool, &identity).await?;
        self.wake.notify_one();
        Ok(queued)
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        index::delete_session_chunks(&self.pool, session_id).await
    }

    pub async fn runtime_status(&self) -> SemanticRuntimeStatus {
        let manager = self.embeddings.read().await;
        let identity = manager.identity();
        let health = manager.healthcheck().await;
        let pending = index::count_chunks(&self.pool, &identity, "pending")
            .await
            .unwrap_or(0);
        let ready = index::count_chunks(&self.pool, &identity, "ready")
            .await
            .unwrap_or(0);
        let local_model_path = manager.local_model_dir();
        let local_model_ready = crate::embedding::local::model_files_present(&local_model_path);
        let status = crate::embedding::semantic_status_from_health(
            manager.settings().enabled,
            pending,
            &health,
        );
        let message = self
            .last_error
            .read()
            .await
            .clone()
            .or_else(|| (!health.ok).then_some(health.message.clone()));
        SemanticRuntimeStatus {
            enabled: manager.settings().enabled,
            status,
            backend: identity.backend,
            model_id: identity.model_id,
            dimensions: Some(identity.dimensions),
            pending_chunks: pending,
            ready_chunks: ready,
            message,
            local_model_ready,
            local_model_path: Some(local_model_path.display().to_string()),
        }
    }

    pub async fn healthcheck(&self) -> EmbeddingHealth {
        self.embeddings.read().await.healthcheck().await
    }

    pub async fn ensure_local_model(&self) -> Result<()> {
        let settings = self.embeddings.read().await.settings().clone();
        if !matches!(settings.backend, crate::models::EmbeddingBackendKind::Local) {
            return Ok(());
        }
        let model_dir = crate::embedding::local_model_dir(&self.data_dir, &settings.local.model);
        let backend =
            crate::embedding::LocalHarrierBackend::open(settings.local.model.clone(), model_dir)
                .await?;
        backend.ensure_model_files().await?;
        self.reload_embeddings(settings).await
    }

    pub async fn import_local_model(&self, path: &Path) -> Result<()> {
        let mut settings = self.embeddings.read().await.settings().clone();
        let model_dir = crate::embedding::local_model_dir(&self.data_dir, &settings.local.model);
        let backend =
            crate::embedding::LocalHarrierBackend::open(settings.local.model.clone(), model_dir)
                .await?;
        backend.import_model_dir(path).await?;
        settings.local.model_path = Some(backend.model_dir().display().to_string());
        self.reload_embeddings(settings).await
    }

    pub async fn search_sessions(&self, query: SearchQuery) -> Result<SessionList> {
        let settings = self.embeddings.read().await.settings().clone();
        let requested = query
            .mode
            .clone()
            .unwrap_or_else(|| settings.default_mode.clone());
        let runtime = self.runtime_status().await;
        let semantic_available = settings.enabled
            && !matches!(
                runtime.status,
                SemanticStatus::Disabled | SemanticStatus::Unavailable
            )
            && runtime.ready_chunks > 0;

        let mut effective_mode = match requested {
            SearchMode::Keyword => SearchMode::Keyword,
            SearchMode::Semantic if semantic_available => SearchMode::Semantic,
            SearchMode::Hybrid if semantic_available => SearchMode::Hybrid,
            SearchMode::Semantic | SearchMode::Hybrid => SearchMode::Keyword,
        };

        let limit = query.limit.unwrap_or(500).clamp(1, 1000);
        let offset = query.offset.unwrap_or(0).max(0);

        let (sessions, total) = match effective_mode {
            SearchMode::Keyword => {
                let total = crate::database::count(&self.pool, &query).await? as usize;
                let sessions = crate::database::search(&self.pool, &query).await?;
                (sessions, total)
            }
            SearchMode::Semantic => {
                let ranked = self.semantic_rank(&query, 500).await?;
                let total = ranked.len();
                let page_ids = ranked
                    .into_iter()
                    .skip(offset as usize)
                    .take(limit as usize)
                    .map(|(id, _)| id)
                    .collect::<Vec<_>>();
                let sessions = index::summaries_by_ids(&self.pool, &page_ids).await?;
                (sessions, total)
            }
            SearchMode::Hybrid => {
                let keyword_rows = crate::database::search(
                    &self.pool,
                    &SearchQuery {
                        limit: Some(200),
                        offset: Some(0),
                        mode: Some(SearchMode::Keyword),
                        ..query.clone()
                    },
                )
                .await?;
                let keyword = keyword_rows
                    .iter()
                    .enumerate()
                    .map(|(rank, session)| (session.id.clone(), 1.0 / (rank as f32 + 1.0)))
                    .collect::<Vec<_>>();
                let semantic = match self.semantic_rank(&query, 200).await {
                    Ok(value) => value,
                    Err(error) => {
                        *self.last_error.write().await = Some(error.to_string());
                        effective_mode = SearchMode::Keyword;
                        Vec::new()
                    }
                };
                if semantic.is_empty() && matches!(effective_mode, SearchMode::Hybrid) {
                    // still hybrid with keyword-only ranks is fine
                }
                let merged = index::reciprocal_rank_fusion(&keyword, &semantic, 60.0);
                let total = merged.len();
                let page_ids = merged
                    .into_iter()
                    .skip(offset as usize)
                    .take(limit as usize)
                    .map(|(id, _)| id)
                    .collect::<Vec<_>>();
                let sessions = if page_ids.is_empty() {
                    crate::database::search(&self.pool, &query).await?
                } else {
                    index::summaries_by_ids(&self.pool, &page_ids).await?
                };
                let total = total.max(sessions.len());
                (sessions, total)
            }
        };

        Ok(SessionList {
            sessions,
            total,
            search_mode: effective_mode,
            semantic_status: runtime.status,
        })
    }

    pub async fn search_session_hits(
        &self,
        session_id: &str,
        query: &str,
        mode: SearchMode,
    ) -> Result<Vec<SessionSearchHit>> {
        let mut hits = if matches!(mode, SearchMode::Keyword | SearchMode::Hybrid) {
            crate::database::search_session_hits(&self.pool, session_id, query).await?
        } else {
            Vec::new()
        };

        if matches!(mode, SearchMode::Semantic | SearchMode::Hybrid) {
            if let Ok(Some(embedding)) = self.embed_query(query).await {
                let identity = self.embeddings.read().await.identity();
                if let Ok(semantic_hits) =
                    index::semantic_session_hits(&self.pool, session_id, &identity, &embedding, 20)
                        .await
                {
                    hits.extend(semantic_hits);
                }
            }
        }
        Ok(hits)
    }

    async fn semantic_rank(&self, query: &SearchQuery, top_k: i64) -> Result<Vec<(String, f32)>> {
        let q = query.q.as_deref().unwrap_or("").trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let Some(embedding) = self.embed_query(q).await? else {
            return Ok(Vec::new());
        };
        let identity = self.embeddings.read().await.identity();
        index::semantic_session_scores(&self.pool, query, &identity, &embedding, top_k).await
    }

    async fn embed_query(&self, query: &str) -> Result<Option<Vec<f32>>> {
        let manager = self.embeddings.read().await;
        if !manager.settings().enabled {
            return Ok(None);
        }
        let backend = manager.active();
        match backend.embed_queries(&[query.to_owned()]).await {
            Ok(mut vectors) => Ok(vectors.pop()),
            Err(error) => {
                *self.last_error.write().await = Some(error.to_string());
                Ok(None)
            }
        }
    }

    async fn worker_loop(self: Arc<Self>) {
        loop {
            self.wake.notified().await;
            if let Err(error) = self.drain_pending().await {
                *self.last_error.write().await = Some(error.to_string());
                tracing::warn!(%error, "semantic index worker failed");
            }
        }
    }

    async fn drain_pending(&self) -> Result<()> {
        let mut running = self.worker_running.lock().await;
        if *running {
            return Ok(());
        }
        *running = true;
        drop(running);

        let result = self.drain_pending_inner().await;

        let mut running = self.worker_running.lock().await;
        *running = false;
        result
    }

    async fn drain_pending_inner(&self) -> Result<()> {
        loop {
            let manager = self.embeddings.read().await;
            if !manager.settings().enabled {
                break;
            }
            let identity = manager.identity();
            let backend = manager.active();
            let pending = index::fetch_pending_chunks(&self.pool, &identity, 8).await?;
            drop(manager);
            if pending.is_empty() {
                break;
            }
            let texts = pending
                .iter()
                .map(|item| item.text.clone())
                .collect::<Vec<_>>();
            let vectors = match backend.embed_documents(&texts).await {
                Ok(vectors) => vectors,
                Err(error) => {
                    for item in &pending {
                        index::mark_chunk_error(&self.pool, item.id, &error.to_string()).await?;
                    }
                    *self.last_error.write().await = Some(error.to_string());
                    break;
                }
            };
            for (item, vector) in pending.into_iter().zip(vectors.into_iter()) {
                if let Some((session_id, message_id, platform)) =
                    index::chunk_meta(&self.pool, item.id).await?
                {
                    index::mark_chunk_ready(
                        &self.pool,
                        item.id,
                        &identity,
                        &session_id,
                        &message_id,
                        &platform,
                        &vector,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }
}
