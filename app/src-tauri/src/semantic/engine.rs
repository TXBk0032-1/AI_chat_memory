use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, RwLock};

use super::index;
use crate::{
    embedding::EmbeddingManager,
    error::Result,
    models::{
        EmbeddingHealth, ReindexProgress, SearchMode, SearchQuery, SemanticRuntimeStatus,
        SemanticStatus, SessionList, SessionSearchHit,
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
    reindex_progress: Arc<RwLock<Option<ReindexProgress>>>,
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
            reindex_progress: Arc::new(RwLock::new(None)),
        }
    }

    pub fn start_worker(self: &Arc<Self>) {
        let engine = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            engine.worker_loop().await;
        });
        self.wake.notify_one();
    }

    pub fn warm_local_model_in_background(self: &Arc<Self>) {
        let engine = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            // Let the window paint and first keyword listing finish first.
            tokio::time::sleep(std::time::Duration::from_millis(750)).await;
            let manager = engine.embeddings.read().await;
            if !matches!(
                manager.settings().backend,
                crate::models::EmbeddingBackendKind::Local
            ) {
                return;
            }
            if manager.is_ready() {
                engine.wake.notify_one();
                return;
            }
            let backend = manager.active();
            drop(manager);
            // Trigger lazy load via a tiny document encode when files already exist.
            match backend.embed_documents(&["warmup".into()]).await {
                Ok(_) => {
                    tracing::info!("local embedding model warmed in background");
                    engine.wake.notify_one();
                }
                Err(error) => {
                    tracing::debug!(%error, "background embedding warm-up skipped");
                }
            }
        });
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
        self.request_reindex_all_with_progress(None).await
    }

    pub async fn request_reindex_all_with_progress(
        &self,
        on_progress: Option<std::sync::Arc<dyn Fn(ReindexProgress) + Send + Sync>>,
    ) -> Result<usize> {
        let identity = self.embeddings.read().await.identity();
        let total_sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);
        let total_sessions = total_sessions.max(0) as usize;
        self.publish_reindex_progress(
            ReindexProgress {
                stage: "queueing".into(),
                total_sessions,
                processed_sessions: 0,
                total_chunks: 0,
                ready_chunks: 0,
                pending_chunks: 0,
                fraction: 0.0,
                message: if total_sessions == 0 {
                    "没有可索引的会话".into()
                } else {
                    format!("正在准备重建索引（0/{total_sessions} 会话）")
                },
            },
            on_progress.as_ref(),
        )
        .await;

        let progress_state = Arc::clone(&self.reindex_progress);
        let progress_cb = on_progress.clone();
        let queued = index::queue_all_sessions_with_progress(
            &self.pool,
            &identity,
            true,
            Some(move |processed_sessions, total_sessions, queued| {
                let fraction = if total_sessions == 0 {
                    1.0
                } else {
                    // Queueing is only the first half of reindex work.
                    (processed_sessions as f32 / total_sessions as f32) * 0.35
                };
                let snapshot = ReindexProgress {
                    stage: "queueing".into(),
                    total_sessions,
                    processed_sessions,
                    total_chunks: queued as i64,
                    ready_chunks: 0,
                    pending_chunks: queued as i64,
                    fraction,
                    message: format!(
                        "正在排队重建索引（{processed_sessions}/{total_sessions} 会话，已标记 {queued} 个 chunk）"
                    ),
                };
                if let Ok(mut guard) = progress_state.try_write() {
                    *guard = Some(snapshot.clone());
                }
                if let Some(callback) = progress_cb.as_ref() {
                    callback(snapshot);
                }
            }),
        )
        .await?;

        let pending = index::count_chunks(&self.pool, &identity, "pending")
            .await
            .unwrap_or(queued as i64);
        let ready = index::count_chunks(&self.pool, &identity, "ready")
            .await
            .unwrap_or(0);
        let total = pending + ready;
        let stage = if pending == 0 { "done" } else { "embedding" };
        let fraction = if pending == 0 {
            1.0
        } else if total > 0 {
            0.35 + (ready as f32 / total as f32) * 0.65
        } else {
            0.35
        };
        self.publish_reindex_progress(
            ReindexProgress {
                stage: stage.into(),
                total_sessions,
                processed_sessions: total_sessions,
                total_chunks: total,
                ready_chunks: ready,
                pending_chunks: pending,
                fraction,
                message: if pending == 0 {
                    "索引已是最新".into()
                } else {
                    format!("排队完成，开始向量化（就绪 {ready}/{total}）")
                },
            },
            on_progress.as_ref(),
        )
        .await;
        self.wake.notify_one();
        Ok(queued)
    }

    async fn publish_reindex_progress(
        &self,
        progress: ReindexProgress,
        on_progress: Option<&std::sync::Arc<dyn Fn(ReindexProgress) + Send + Sync>>,
    ) {
        *self.reindex_progress.write().await = Some(progress.clone());
        if let Some(callback) = on_progress {
            callback(progress);
        }
    }

    async fn note_embedding_progress(&self) {
        let previous = self.reindex_progress.read().await.clone();
        let Some(previous) = previous else {
            return;
        };
        // Only keep updating while a rebuild is in flight.
        if previous.stage == "done" || previous.stage == "error" {
            return;
        }
        let identity = self.embeddings.read().await.identity();
        let pending = index::count_chunks(&self.pool, &identity, "pending")
            .await
            .unwrap_or(0);
        let ready = index::count_chunks(&self.pool, &identity, "ready")
            .await
            .unwrap_or(0);
        let total = pending + ready;
        let fraction = if total == 0 {
            1.0
        } else {
            0.35 + (ready as f32 / total as f32) * 0.65
        };
        let progress = ReindexProgress {
            stage: if pending == 0 {
                "done".into()
            } else {
                "embedding".into()
            },
            total_sessions: previous.total_sessions,
            processed_sessions: previous.processed_sessions,
            total_chunks: total,
            ready_chunks: ready,
            pending_chunks: pending,
            fraction: fraction.clamp(0.0, 1.0),
            message: if pending == 0 {
                format!("重建索引完成（就绪 {ready}）")
            } else {
                format!("正在向量化（就绪 {ready}/{total}，剩余 {pending}）")
            },
        };
        *self.reindex_progress.write().await = Some(progress);
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
        let reindex = self.reindex_progress.read().await.clone();
        let last_error = self.last_error.read().await.clone();
        let message = reindex
            .as_ref()
            .map(|item| item.message.clone())
            .or(last_error)
            .or_else(|| (!health.ok).then_some(health.message.clone()));
        let active = manager.active();
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
            device: active.runtime_device(),
            dtype: active.runtime_dtype(),
            reindex,
        }
    }

    pub async fn healthcheck(&self) -> EmbeddingHealth {
        self.embeddings.read().await.healthcheck().await
    }

    pub async fn ensure_local_model(
        &self,
        on_progress: Option<crate::embedding::local::DownloadProgressCallback>,
    ) -> Result<()> {
        let settings = self.embeddings.read().await.settings().clone();
        if !matches!(settings.backend, crate::models::EmbeddingBackendKind::Local) {
            return Ok(());
        }
        let model_dir = crate::embedding::local_model_dir(&self.data_dir, &settings.local.model);
        let backend = crate::embedding::LocalHarrierBackend::open(
            settings.local.model.clone(),
            model_dir,
            &settings.local,
        )
        .await?;
        backend
            .ensure_model_files_with_progress(on_progress)
            .await?;
        *self.last_error.write().await = None;
        // reload_embeddings also queues a full reindex and wakes the worker.
        self.reload_embeddings(settings).await
    }

    pub async fn import_local_model(&self, path: &Path) -> Result<()> {
        let mut settings = self.embeddings.read().await.settings().clone();
        let model_dir = crate::embedding::local_model_dir(&self.data_dir, &settings.local.model);
        let backend = crate::embedding::LocalHarrierBackend::open(
            settings.local.model.clone(),
            model_dir,
            &settings.local,
        )
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
        let backend_ready = self.embeddings.read().await.is_ready();
        let semantic_available = settings.enabled
            && backend_ready
            && !matches!(
                runtime.status,
                SemanticStatus::Disabled | SemanticStatus::Unavailable
            )
            && runtime.ready_chunks > 0;

        // Empty listing is pure keyword work; do not wait for local model warm-up.
        let listing_only = query.q.as_deref().map(str::trim).unwrap_or("").is_empty();

        let mut effective_mode = if listing_only {
            SearchMode::Keyword
        } else {
            match requested {
                SearchMode::Keyword => SearchMode::Keyword,
                SearchMode::Semantic if semantic_available => SearchMode::Semantic,
                SearchMode::Hybrid if semantic_available => SearchMode::Hybrid,
                SearchMode::Semantic | SearchMode::Hybrid => SearchMode::Keyword,
            }
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

        if matches!(mode, SearchMode::Semantic | SearchMode::Hybrid)
            && let Ok(Some(embedding)) = self.embed_query(query).await
        {
            let identity = self.embeddings.read().await.identity();
            if let Ok(semantic_hits) =
                index::semantic_session_hits(&self.pool, session_id, &identity, &embedding, 20)
                    .await
            {
                hits.extend(semantic_hits);
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
        // Avoid blocking the first UI search while the 500MB local model is still loading.
        if !manager.is_ready() {
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
            // While the local model is still warming, leave pending chunks alone so
            // UI-facing requests keep the async runtime free.
            if !manager.is_ready()
                && matches!(
                    manager.settings().backend,
                    crate::models::EmbeddingBackendKind::Local
                )
            {
                break;
            }
            let identity = manager.identity();
            let backend = manager.active();
            let is_local = matches!(identity.backend, crate::models::EmbeddingBackendKind::Local);
            let fetch_limit = if is_local {
                crate::embedding::local::LOCAL_INDEX_CANDIDATE_LIMIT
            } else {
                16
            };
            let candidates =
                index::fetch_pending_chunks(&self.pool, &identity, fetch_limit).await?;
            drop(manager);
            if candidates.is_empty() {
                break;
            }
            let pending = if is_local {
                let estimates = candidates
                    .iter()
                    .map(|item| crate::embedding::local::estimate_token_count(&item.text))
                    .collect::<Vec<_>>();
                let chosen = crate::embedding::local::plan_local_index_batch(&estimates);
                let est_tokens: usize = chosen.iter().map(|&i| estimates[i]).sum();
                let est_max = chosen.iter().map(|&i| estimates[i]).max().unwrap_or(0);
                let est_pad = if chosen.is_empty() || est_max == 0 {
                    0.0
                } else {
                    1.0 - (est_tokens as f64 / (chosen.len() as f64 * est_max as f64))
                };
                tracing::info!(
                    candidates = candidates.len(),
                    chosen = chosen.len(),
                    est_tokens,
                    est_max_len = est_max,
                    est_pad_ratio = est_pad,
                    token_budget = crate::embedding::local::LOCAL_INDEX_TOKEN_BUDGET,
                    "local index batch planned"
                );
                chosen
                    .into_iter()
                    .map(|idx| candidates[idx].clone())
                    .collect::<Vec<_>>()
            } else {
                candidates
            };
            if pending.is_empty() {
                break;
            }
            let texts = pending
                .iter()
                .map(|item| item.text.clone())
                .collect::<Vec<_>>();
            let started = std::time::Instant::now();
            let embed_started = std::time::Instant::now();
            let vectors = match backend.embed_documents(&texts).await {
                Ok(vectors) => {
                    *self.last_error.write().await = None;
                    vectors
                }
                Err(error) => {
                    // Keep chunks pending so a later successful model load can resume.
                    *self.last_error.write().await = Some(error.to_string());
                    tracing::warn!(%error, pending = pending.len(), "semantic embedding failed; will retry later");
                    // Avoid a tight spin while CUDA recovers from bad batches.
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    break;
                }
            };
            let embed_ms = embed_started.elapsed().as_millis();
            let ready_items = pending
                .iter()
                .zip(vectors)
                .map(|(item, vector)| {
                    (
                        item.id,
                        item.session_id.as_str(),
                        item.message_id.as_str(),
                        item.platform.as_str(),
                        vector,
                    )
                })
                .collect::<Vec<_>>();
            let write_started = std::time::Instant::now();
            index::mark_chunks_ready(&self.pool, &identity, &ready_items).await?;
            let write_ms = write_started.elapsed().as_millis();
            let elapsed_ms = started.elapsed().as_millis();
            let chunks_per_sec = if elapsed_ms == 0 {
                pending.len() as f64
            } else {
                (pending.len() as f64) * 1000.0 / (elapsed_ms as f64)
            };
            tracing::info!(
                batch_size = pending.len(),
                device = backend.runtime_device().as_deref().unwrap_or("unknown"),
                dtype = backend.runtime_dtype().as_deref().unwrap_or("unknown"),
                embed_ms,
                write_ms,
                elapsed_ms,
                chunks_per_sec,
                "semantic embedding batch completed"
            );
            self.note_embedding_progress().await;
        }
        Ok(())
    }
}
