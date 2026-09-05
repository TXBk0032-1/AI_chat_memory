use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Mutex, Notify, RwLock};

use super::index;
use crate::{
    embedding::EmbeddingManager,
    error::{AppError, Result},
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
    worker_gate: Arc<Mutex<()>>,
    reload_gate: Arc<Mutex<()>>,
    generation: Arc<AtomicU64>,
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
            worker_gate: Arc::new(Mutex::new(())),
            reload_gate: Arc::new(Mutex::new(())),
            generation: Arc::new(AtomicU64::new(0)),
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

    pub async fn reload_embeddings(
        &self,
        settings: crate::models::SemanticSearchSettings,
    ) -> Result<()> {
        let _reload = self.reload_gate.lock().await;
        let manager = EmbeddingManager::from_settings(self.data_dir.clone(), settings).await?;
        // Invalidate and cancel the old backend before waiting for its current batch.
        // Holding worker_gate through activation prevents another old-backend batch
        // from starting between the wait and the manager swap.
        self.embeddings.read().await.request_cancel();
        self.invalidate_generation();
        let _worker = self.worker_gate.lock().await;
        let identity = manager.identity();
        crate::database::connection::ensure_embedding_vec_table(
            &self.pool,
            Some(identity.dimensions),
        )
        .await?;
        crate::database::connection::activate_embedding_index(
            &self.pool,
            &identity.backend_id,
            &identity.model_id,
        )
        .await?;
        *self.embeddings.write().await = manager;
        self.request_reindex_all().await?;
        self.wake.notify_one();
        Ok(())
    }

    fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn invalidate_generation(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    async fn batch_matches_current(
        &self,
        generation: u64,
        identity: &crate::embedding::BackendIdentity,
    ) -> bool {
        if self.current_generation() != generation {
            return false;
        }
        let current = self.embeddings.read().await.identity();
        current.backend == identity.backend
            && current.backend_id == identity.backend_id
            && current.model_id == identity.model_id
            && current.dimensions == identity.dimensions
    }

    pub async fn cancel_semantic_work(&self) -> Result<()> {
        let manager = self.embeddings.read().await;
        manager.request_cancel();
        self.publish_reindex_progress(
            crate::models::ReindexProgress {
                stage: "cancelled".into(),
                total_sessions: 0,
                processed_sessions: 0,
                total_chunks: 0,
                ready_chunks: 0,
                pending_chunks: 0,
                fraction: 0.0,
                message: "已取消下载/索引任务".into(),
            },
            None,
        )
        .await;
        Ok(())
    }

    pub async fn clear_cancel_flag(&self) {
        self.embeddings.read().await.clear_cancel();
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
        {
            let manager = self.embeddings.read().await;
            manager.clear_cancel();
            crate::database::connection::ensure_embedding_vec_table(
                &self.pool,
                Some(manager.identity().dimensions),
            )
            .await?;
        }
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
        let health = manager.status_snapshot();
        let pending = index::count_chunks(&self.pool, &identity, "pending")
            .await
            .unwrap_or(0);
        let ready = index::count_chunks(&self.pool, &identity, "ready")
            .await
            .unwrap_or(0);
        let local_model_path = manager.local_model_dir();
        let local_model_ready = crate::embedding::local_model_files_present(&local_model_path);
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
        self.clear_cancel_flag().await;

        let settings = self.embeddings.read().await.settings().clone();
        if !matches!(settings.backend, crate::models::EmbeddingBackendKind::Local) {
            return Ok(());
        }
        let model_dir = crate::embedding::local_model_dir(&self.data_dir, &settings.local.model);
        let cancel = self.embeddings.read().await.cancel_flag();
        if crate::embedding::bge::is_bge_model(&settings.local.model) {
            let backend = crate::embedding::LocalBgeBackend::open(
                settings.local.model.clone(),
                model_dir,
                &settings.local,
                cancel,
            )
            .await?;
            backend
                .ensure_model_files_with_progress(on_progress)
                .await?;
        } else {
            let backend = crate::embedding::LocalHarrierBackend::open(
                settings.local.model.clone(),
                model_dir,
                &settings.local,
                cancel,
            )
            .await?;
            backend
                .ensure_model_files_with_progress(on_progress)
                .await?;
        }
        *self.last_error.write().await = None;
        // reload_embeddings also queues a full reindex and wakes the worker.
        self.reload_embeddings(settings).await
    }

    pub async fn import_local_model(&self, path: &Path) -> Result<()> {
        let mut settings = self.embeddings.read().await.settings().clone();
        let model_dir = crate::embedding::local_model_dir(&self.data_dir, &settings.local.model);
        let cancel = self.embeddings.read().await.cancel_flag();
        if crate::embedding::bge::is_bge_model(&settings.local.model) {
            let backend = crate::embedding::LocalBgeBackend::open(
                settings.local.model.clone(),
                model_dir,
                &settings.local,
                cancel,
            )
            .await?;
            backend.import_from_path(path).await?;
            settings.local.model_path = Some(backend.model_dir().display().to_string());
        } else {
            let backend = crate::embedding::LocalHarrierBackend::open(
                settings.local.model.clone(),
                model_dir,
                &settings.local,
                cancel,
            )
            .await?;
            backend.import_model_dir(path).await?;
            settings.local.model_path = Some(backend.model_dir().display().to_string());
        }
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
                let (sessions, total) =
                    crate::database::search_and_count(&self.pool, &query).await?;
                (sessions, total as usize)
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
        let mut seen = std::collections::HashSet::new();
        let mut hits = Vec::new();

        if matches!(mode, SearchMode::Keyword | SearchMode::Hybrid) {
            for hit in crate::database::search_session_hits(&self.pool, session_id, query).await? {
                if seen.insert(hit.message_id.clone()) {
                    hits.push(hit);
                }
            }
        }

        if matches!(mode, SearchMode::Semantic | SearchMode::Hybrid)
            && let Ok(Some(embedding)) = self.embed_query(query).await
        {
            let identity = self.embeddings.read().await.identity();
            if let Ok(semantic_hits) =
                index::semantic_session_hits(&self.pool, session_id, &identity, &embedding, 20)
                    .await
            {
                for hit in semantic_hits {
                    if seen.insert(hit.message_id.clone()) {
                        hits.push(hit);
                    }
                }
            }
        }
        hits.sort_by_key(|h| h.seq);
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
                tracing::warn!(%error, "semantic index worker failed; scheduling retry");
                // drain_pending propagates transient embed failures as Err
                // (user cancellation still returns Ok). Pending chunks stay
                // queued and nothing else re-wakes the worker, so schedule a
                // self-retry to resume after a transient CUDA/HTTP failure
                // instead of stalling until the next external import or reindex.
                let engine = Arc::clone(&self);
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    engine.wake.notify_one();
                });
            }
        }
    }

    async fn drain_pending(&self) -> Result<()> {
        let _worker = self.worker_gate.lock().await;
        self.drain_pending_inner().await
    }

    async fn drain_pending_inner(&self) -> Result<()> {
        let generation = self.current_generation();
        'fetch: loop {
            if self.current_generation() != generation {
                break;
            }
            let manager = self.embeddings.read().await;
            if !manager.settings().enabled {
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
            // Local backends pack one length band per call; planning all packs
            // for the candidate window up front lets a single wake embed every
            // pending chunk instead of re-fetching and re-estimating the whole
            // window once per band.
            let batches: Vec<Vec<index::PendingChunk>> = if is_local {
                let estimates = candidates
                    .iter()
                    .map(|item| crate::embedding::local::estimate_token_count(&item.text))
                    .collect::<Vec<_>>();
                let packs = crate::embedding::local::plan_local_index_batches(&estimates);
                let planned: usize = packs.iter().map(Vec::len).sum();
                let est_tokens: usize = packs.iter().flatten().map(|&idx| estimates[idx]).sum();
                tracing::info!(
                    candidates = candidates.len(),
                    packs = packs.len(),
                    planned,
                    est_tokens,
                    token_budget = crate::embedding::local::LOCAL_INDEX_TOKEN_BUDGET,
                    "local index window planned"
                );
                packs
                    .into_iter()
                    .map(|pack| {
                        pack.into_iter()
                            .map(|idx| candidates[idx].clone())
                            .collect::<Vec<_>>()
                    })
                    .collect()
            } else {
                vec![candidates]
            };
            for pending in batches {
                if pending.is_empty() {
                    continue;
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
                        // Classify by error type, not message text: local
                        // backends signal user cancellation via
                        // AppError::Cancelled, while everything else (CUDA
                        // OOM, HTTP timeouts, 5xx) is a transient failure that
                        // must propagate so worker_loop's existing 5s self-heal
                        // timer re-wakes the drain.
                        if matches!(error, AppError::Cancelled(_)) {
                            tracing::info!(%error, pending = pending.len(), "semantic embedding cancelled");
                            self.publish_reindex_progress(
                                ReindexProgress {
                                    stage: "cancelled".into(),
                                    total_sessions: 0,
                                    processed_sessions: 0,
                                    total_chunks: 0,
                                    ready_chunks: 0,
                                    pending_chunks: pending.len() as i64,
                                    fraction: 0.0,
                                    message: "索引编码已取消".into(),
                                },
                                None,
                            )
                            .await;
                            break 'fetch;
                        }
                        // Keep chunks pending so a later successful model load can resume.
                        tracing::warn!(%error, pending = pending.len(), "semantic embedding failed; scheduling self retry");
                        return Err(error);
                    }
                };
                let embed_ms = embed_started.elapsed().as_millis();
                // HTTP backends may discover dimensions while serving this request, so
                // compare the post-inference identity with the current manager.
                let write_identity = backend.identity();
                if !self
                    .batch_matches_current(generation, &write_identity)
                    .await
                {
                    tracing::info!(
                        generation,
                        backend = %write_identity.backend_id,
                        model = %write_identity.model_id,
                        "discarding stale embedding batch after backend switch"
                    );
                    break 'fetch;
                }
                // HTTP backends may discover their actual dimensions from the first response.
                // Recreate vec0 before writing rather than padding/truncating to a stale guess.
                if write_identity.dimensions != identity.dimensions {
                    crate::database::connection::ensure_embedding_vec_table(
                        &self.pool,
                        Some(write_identity.dimensions),
                    )
                    .await?;
                    tracing::info!(
                        previous = identity.dimensions,
                        actual = write_identity.dimensions,
                        model = %write_identity.model_id,
                        "embedding endpoint dimensions discovered"
                    );
                }
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
                index::mark_chunks_ready(&self.pool, &write_identity, &ready_items).await?;
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
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::{
        embedding::{BackendIdentity, EmbeddingBackend},
        error::Result,
        models::{EmbeddingBackendKind, EmbeddingHealth, SemanticSearchSettings},
    };

    struct CountingBackend {
        healthchecks: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl EmbeddingBackend for CountingBackend {
        fn identity(&self) -> BackendIdentity {
            BackendIdentity {
                backend: EmbeddingBackendKind::Ollama,
                backend_id: "counting".into(),
                model_id: "counting-model".into(),
                dimensions: 8,
            }
        }

        async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(vec![vec![0.0; 8]; texts.len()])
        }

        async fn embed_queries(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(vec![vec![0.0; 8]; texts.len()])
        }

        async fn healthcheck(&self) -> Result<EmbeddingHealth> {
            self.healthchecks.fetch_add(1, Ordering::SeqCst);
            Ok(EmbeddingHealth {
                ok: true,
                backend: EmbeddingBackendKind::Ollama,
                model_id: "counting-model".into(),
                dimensions: Some(8),
                message: "ok".into(),
            })
        }
    }

    #[tokio::test]
    async fn runtime_status_does_not_execute_backend_healthcheck() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE embedding_chunks (
                backend_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                status TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        let healthchecks = Arc::new(AtomicUsize::new(0));
        let settings = SemanticSearchSettings {
            enabled: true,
            backend: EmbeddingBackendKind::Ollama,
            ..SemanticSearchSettings::default()
        };
        let manager = EmbeddingManager::from_backend_for_test(
            std::env::temp_dir().join("ai-chat-memory-runtime-status-test"),
            settings,
            Arc::new(CountingBackend {
                healthchecks: healthchecks.clone(),
            }),
        );
        let engine = SemanticEngine::new(pool, std::env::temp_dir(), manager);

        let status = engine.runtime_status().await;

        assert_eq!(status.model_id, "counting-model");
        assert_eq!(healthchecks.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn invalidated_generation_rejects_old_embedding_batch() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let settings = SemanticSearchSettings {
            enabled: true,
            backend: EmbeddingBackendKind::Ollama,
            ..SemanticSearchSettings::default()
        };
        let manager = EmbeddingManager::from_backend_for_test(
            std::env::temp_dir(),
            settings,
            Arc::new(CountingBackend {
                healthchecks: Arc::new(AtomicUsize::new(0)),
            }),
        );
        let engine = SemanticEngine::new(pool, std::env::temp_dir(), manager);
        let generation = engine.current_generation();
        let identity = engine.embeddings.read().await.identity();
        assert!(engine.batch_matches_current(generation, &identity).await);

        engine.invalidate_generation();

        assert!(!engine.batch_matches_current(generation, &identity).await);
    }

    #[tokio::test]
    async fn changed_backend_identity_rejects_old_embedding_batch() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let settings = SemanticSearchSettings {
            enabled: true,
            backend: EmbeddingBackendKind::Ollama,
            ..SemanticSearchSettings::default()
        };
        let manager = EmbeddingManager::from_backend_for_test(
            std::env::temp_dir(),
            settings.clone(),
            Arc::new(CountingBackend {
                healthchecks: Arc::new(AtomicUsize::new(0)),
            }),
        );
        let engine = SemanticEngine::new(pool, std::env::temp_dir(), manager);
        let generation = engine.current_generation();
        let old_identity = engine.embeddings.read().await.identity();
        *engine.embeddings.write().await = EmbeddingManager::from_backend_for_test(
            std::env::temp_dir(),
            settings,
            Arc::new(crate::embedding::MockEmbeddingBackend::new(
                EmbeddingBackendKind::Ollama,
                "replacement-model".into(),
                8,
            )),
        );

        assert!(
            !engine
                .batch_matches_current(generation, &old_identity)
                .await
        );
    }

    /// Local-kind backend so the multi-pack local path is exercised.
    struct LocalKindBackend {
        embed_calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl EmbeddingBackend for LocalKindBackend {
        fn identity(&self) -> BackendIdentity {
            BackendIdentity {
                backend: EmbeddingBackendKind::Local,
                backend_id: "local".into(),
                model_id: "test".into(),
                dimensions: 8,
            }
        }

        async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            self.embed_calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![vec![0.0; 8]; texts.len()])
        }

        async fn embed_queries(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(vec![vec![0.0; 8]; texts.len()])
        }

        async fn healthcheck(&self) -> Result<EmbeddingHealth> {
            Ok(EmbeddingHealth {
                ok: true,
                backend: EmbeddingBackendKind::Local,
                model_id: "test".into(),
                dimensions: Some(8),
                message: "ok".into(),
            })
        }
    }

    #[tokio::test]
    async fn drain_pending_processes_every_pending_chunk_in_one_wake() {
        crate::database::connection::register_sqlite_vec();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::raw_sql(
            "CREATE TABLE embedding_chunks (
                id INTEGER PRIMARY KEY,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                platform TEXT NOT NULL,
                backend_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                status TEXT NOT NULL,
                error TEXT,
                dim INTEGER,
                updated_at TEXT NOT NULL,
                text TEXT NOT NULL
            );
             CREATE VIRTUAL TABLE embedding_vec USING vec0(
                chunk_id INTEGER PRIMARY KEY,
                embedding float[8] distance_metric=cosine,
                +session_id TEXT,
                +message_id TEXT,
                +platform TEXT
             );",
        )
        .execute(&pool)
        .await
        .unwrap();
        // Mixed length bands force several packs per candidate window.
        for chunk_id in 1..=15_i64 {
            sqlx::query(
                "INSERT INTO embedding_chunks
                 (id, message_id, session_id, platform, backend_id, model_id, status, updated_at, text)
                 VALUES (?, ?, 's1', 'test', 'local', 'test', 'pending', 'now', ?)",
            )
            .bind(chunk_id)
            .bind(format!("msg-{chunk_id}"))
            .bind(format!("short text {chunk_id}"))
            .execute(&pool)
            .await
            .unwrap();
        }
        for chunk_id in 16..=30_i64 {
            sqlx::query(
                "INSERT INTO embedding_chunks
                 (id, message_id, session_id, platform, backend_id, model_id, status, updated_at, text)
                 VALUES (?, ?, 's1', 'test', 'local', 'test', 'pending', 'now', ?)",
            )
            .bind(chunk_id)
            .bind(format!("msg-{chunk_id}"))
            .bind("long".repeat(4_000))
            .execute(&pool)
            .await
            .unwrap();
        }

        let settings = SemanticSearchSettings {
            enabled: true,
            backend: EmbeddingBackendKind::Local,
            ..SemanticSearchSettings::default()
        };
        let embed_calls = Arc::new(AtomicUsize::new(0));
        let manager = EmbeddingManager::from_backend_for_test(
            std::env::temp_dir(),
            settings,
            Arc::new(LocalKindBackend {
                embed_calls: embed_calls.clone(),
            }),
        );
        let engine = SemanticEngine::new(pool.clone(), std::env::temp_dir(), manager);

        engine.drain_pending().await.unwrap();

        let identity = engine.embeddings.read().await.identity();
        let ready = index::count_chunks(&pool, &identity, "ready")
            .await
            .unwrap();
        let pending = index::count_chunks(&pool, &identity, "pending")
            .await
            .unwrap();
        assert_eq!(
            ready, 30,
            "every pending chunk must be vectorized in one wake"
        );
        assert_eq!(
            pending, 0,
            "no candidate may be silently left for another wake"
        );
        // Mixed bands require more than one consecutive embed call.
        assert!(
            embed_calls.load(Ordering::SeqCst) >= 2,
            "expected multiple packs, got {}",
            embed_calls.load(Ordering::SeqCst)
        );
    }
}
