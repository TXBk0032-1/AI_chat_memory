use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{
    error::{AppError, Result},
    models::{EmbeddingBackendKind, EmbeddingHealth, SemanticSearchSettings, SemanticStatus},
};

pub mod bge;
mod http;
pub mod local;
mod mock;

pub use bge::LocalBgeBackend;
pub use http::HttpEmbeddingBackend;
pub use local::LocalHarrierBackend;
pub use mock::MockEmbeddingBackend;

#[derive(Debug, Clone)]
pub struct BackendIdentity {
    pub backend: EmbeddingBackendKind,
    pub backend_id: String,
    pub model_id: String,
    pub dimensions: usize,
}

#[async_trait]
pub trait EmbeddingBackend: Send + Sync {
    fn identity(&self) -> BackendIdentity;
    /// True when the backend can answer embedding requests without a heavy first-load stall.
    fn is_ready(&self) -> bool {
        true
    }
    fn runtime_device(&self) -> Option<String> {
        None
    }
    fn runtime_dtype(&self) -> Option<String> {
        None
    }
    async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    async fn embed_queries(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    async fn healthcheck(&self) -> Result<EmbeddingHealth>;
}

pub struct EmbeddingManager {
    data_dir: PathBuf,
    settings: SemanticSearchSettings,
    active: Arc<dyn EmbeddingBackend>,
    cancel_flag: Arc<AtomicBool>,
}

impl EmbeddingManager {
    pub async fn from_settings(
        data_dir: PathBuf,
        settings: SemanticSearchSettings,
    ) -> Result<Self> {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let active = build_backend(&data_dir, &settings, cancel_flag.clone()).await?;
        Ok(Self {
            data_dir,
            settings,
            active,
            cancel_flag,
        })
    }

    pub fn settings(&self) -> &SemanticSearchSettings {
        &self.settings
    }

    pub fn active(&self) -> Arc<dyn EmbeddingBackend> {
        self.active.clone()
    }

    pub fn is_ready(&self) -> bool {
        self.active.is_ready()
    }

    pub fn identity(&self) -> BackendIdentity {
        self.active.identity()
    }

    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel_flag.clone()
    }

    pub fn request_cancel(&self) {
        self.cancel_flag.store(true, Ordering::SeqCst);
    }

    pub fn clear_cancel(&self) {
        self.cancel_flag.store(false, Ordering::SeqCst);
    }

    pub async fn healthcheck(&self) -> EmbeddingHealth {
        match self.active.healthcheck().await {
            Ok(value) => value,
            Err(error) => EmbeddingHealth {
                ok: false,
                backend: self.settings.backend.clone(),
                model_id: self.identity().model_id,
                dimensions: Some(self.identity().dimensions),
                message: error.to_string(),
            },
        }
    }

    pub fn local_model_dir(&self) -> PathBuf {
        local_model_dir(&self.data_dir, &self.settings.local.model)
    }
}

pub async fn build_backend(
    data_dir: &Path,
    settings: &SemanticSearchSettings,
    cancel_flag: Arc<AtomicBool>,
) -> Result<Arc<dyn EmbeddingBackend>> {
    match settings.backend {
        EmbeddingBackendKind::Local => {
            let model_dir = settings
                .local
                .model_path
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(|| local_model_dir(data_dir, &settings.local.model));
            if bge::is_bge_model(&settings.local.model) {
                match LocalBgeBackend::open(
                    settings.local.model.clone(),
                    model_dir,
                    &settings.local,
                    cancel_flag,
                )
                .await
                {
                    Ok(backend) => Ok(Arc::new(backend)),
                    Err(error) => {
                        tracing::warn!(%error, "local bge backend unavailable; using deterministic mock until model is ready");
                        Ok(Arc::new(MockEmbeddingBackend::new(
                            EmbeddingBackendKind::Local,
                            settings.local.model.clone(),
                            512,
                        )))
                    }
                }
            } else {
                match LocalHarrierBackend::open(
                    settings.local.model.clone(),
                    model_dir,
                    &settings.local,
                    cancel_flag,
                )
                .await
                {
                    Ok(backend) => Ok(Arc::new(backend)),
                    Err(error) => {
                        tracing::warn!(%error, "local embedding backend unavailable; using deterministic mock until model is ready");
                        Ok(Arc::new(MockEmbeddingBackend::new(
                            EmbeddingBackendKind::Local,
                            settings.local.model.clone(),
                            640,
                        )))
                    }
                }
            }
        }
        EmbeddingBackendKind::Ollama => Ok(Arc::new(HttpEmbeddingBackend::ollama(
            settings.ollama.clone(),
        )?)),
        EmbeddingBackendKind::LlamaCpp => Ok(Arc::new(HttpEmbeddingBackend::openai_compatible(
            EmbeddingBackendKind::LlamaCpp,
            settings.llama_cpp.clone(),
        )?)),
        EmbeddingBackendKind::OpenaiCompatible => {
            Ok(Arc::new(HttpEmbeddingBackend::openai_compatible(
                EmbeddingBackendKind::OpenaiCompatible,
                settings.openai_compatible.clone(),
            )?))
        }
    }
}

pub fn local_model_dir(data_dir: &Path, model: &str) -> PathBuf {
    let safe = model.replace(['/', '\\', ':'], "__");
    data_dir.join("models").join(safe)
}

pub fn semantic_status_from_health(
    enabled: bool,
    pending_chunks: i64,
    health: &EmbeddingHealth,
) -> SemanticStatus {
    if !enabled {
        SemanticStatus::Disabled
    } else if pending_chunks > 0 {
        SemanticStatus::Indexing
    } else if health.ok {
        SemanticStatus::Ready
    } else {
        SemanticStatus::Unavailable
    }
}

pub fn ensure_dimensions(vectors: &[Vec<f32>], expected: usize) -> Result<()> {
    for vector in vectors {
        if vector.len() != expected {
            return Err(AppError::Configuration(format!(
                "embedding dimension mismatch: expected {expected}, got {}",
                vector.len()
            )));
        }
    }
    Ok(())
}
