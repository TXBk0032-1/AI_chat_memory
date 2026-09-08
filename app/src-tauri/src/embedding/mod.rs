use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::{
    error::{AppError, Result},
    models::{EmbeddingBackendKind, EmbeddingHealth, SemanticSearchSettings, SemanticStatus},
};

pub mod bge;
pub mod cancellation;
mod http;
pub mod local;
// The deterministic mock backend is a test fixture only: since the silent
// mock fallback was removed, no production path constructs it.
#[cfg(test)]
mod mock;

pub use bge::LocalBgeBackend;
pub use cancellation::{CancellationSource, CancellationToken};
pub use http::HttpEmbeddingBackend;
pub use local::LocalHarrierBackend;
#[cfg(test)]
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
    /// Best-effort background warm-up: pre-loads heavy local resources so the
    /// first embed/search does not pay a load stall. The default is a no-op
    /// for backends with nothing to pre-load (HTTP) or that already load
    /// lazily on first use.
    async fn warm_up(&self) -> Result<()> {
        Ok(())
    }
    fn runtime_device(&self) -> Option<String> {
        None
    }
    fn runtime_dtype(&self) -> Option<String> {
        None
    }
    /// Embeds documents for the background index. `cancellation` is the
    /// caller's token for this unit of work; backends must abort with
    /// `AppError::Cancelled` once it reports cancelled and must not consult
    /// any other cancellation state.
    async fn embed_documents(
        &self,
        texts: &[String],
        cancellation: Option<&CancellationToken>,
    ) -> Result<Vec<Vec<f32>>>;
    /// Embeds interactive search queries. Never subject to background
    /// cancellation.
    async fn embed_queries(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    async fn healthcheck(&self) -> Result<EmbeddingHealth>;
}

pub struct EmbeddingManager {
    data_dir: PathBuf,
    settings: SemanticSearchSettings,
    active: Arc<dyn EmbeddingBackend>,
    cancellation: CancellationSource,
}

impl EmbeddingManager {
    #[cfg(test)]
    pub(crate) fn from_backend_for_test(
        data_dir: PathBuf,
        settings: SemanticSearchSettings,
        active: Arc<dyn EmbeddingBackend>,
    ) -> Self {
        Self {
            data_dir,
            settings,
            active,
            cancellation: CancellationSource::new(),
        }
    }

    pub async fn from_settings(
        data_dir: PathBuf,
        settings: SemanticSearchSettings,
    ) -> Result<Self> {
        let cancellation = CancellationSource::new();
        let active = build_backend(&data_dir, &settings, cancellation.clone()).await?;
        Ok(Self {
            data_dir,
            settings,
            active,
            cancellation,
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

    /// Shared cancellation source, for local backends that check cancellation
    /// during model download / load outside of an `embed_documents` call.
    pub fn cancellation_source(&self) -> CancellationSource {
        self.cancellation.clone()
    }

    /// Token for a new unit of background work (index drain). Cancelled only
    /// by a `request_cancel()` that happens after it was issued.
    pub fn background_token(&self) -> CancellationToken {
        self.cancellation.begin_work()
    }

    /// Cancels every background token issued so far. Work started afterwards
    /// is unaffected; there is deliberately no way to "clear" a cancellation.
    pub fn request_cancel(&self) {
        self.cancellation.cancel_current();
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

    pub fn status_snapshot(&self) -> EmbeddingHealth {
        let identity = self.identity();
        let available = match self.settings.backend {
            EmbeddingBackendKind::Local => local_model_files_present(&self.local_model_dir()),
            _ => true,
        };
        EmbeddingHealth {
            ok: available,
            backend: identity.backend,
            model_id: identity.model_id,
            dimensions: Some(identity.dimensions),
            message: if !available {
                format!("模型文件未就绪：{}", self.local_model_dir().display())
            } else if self.is_ready() {
                "后端已就绪".into()
            } else {
                "模型文件已就绪（按需加载）".into()
            },
        }
    }

    pub fn local_model_dir(&self) -> PathBuf {
        self.settings
            .local
            .model_path
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| local_model_dir(&self.data_dir, &self.settings.local.model))
    }
}

/// Shared readiness check for local model files, used by status_snapshot and
/// runtime status so both supported local model families are treated the same.
pub fn local_model_files_present(model_dir: &Path) -> bool {
    bge::model_files_present(model_dir) || local::model_files_present(model_dir)
}

pub async fn build_backend(
    data_dir: &Path,
    settings: &SemanticSearchSettings,
    cancellation: CancellationSource,
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
                Ok(Arc::new(
                    LocalBgeBackend::open(
                        settings.local.model.clone(),
                        model_dir,
                        &settings.local,
                        cancellation,
                    )
                    .await?,
                ))
            } else {
                // No silent mock fallback: a backend that fails to open must
                // propagate the error so the index is never built on
                // deterministic fake vectors. (Missing model files are not an
                // open failure — the harrier backend loads lazily on first
                // use and reports standby through status_snapshot.)
                Ok(Arc::new(
                    LocalHarrierBackend::open(
                        settings.local.model.clone(),
                        model_dir,
                        &settings.local,
                        cancellation,
                    )
                    .await?,
                ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{LocalEmbeddingSettings, SemanticSearchSettings};

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ai-chat-memory-embedding-{name}-{}",
            std::process::id()
        ))
    }

    fn local_settings(model_path: &Path) -> SemanticSearchSettings {
        SemanticSearchSettings {
            backend: EmbeddingBackendKind::Local,
            local: LocalEmbeddingSettings {
                model: "test/harrier-fixture".into(),
                model_path: Some(model_path.display().to_string()),
                ..LocalEmbeddingSettings::default()
            },
            ..SemanticSearchSettings::default()
        }
    }

    #[tokio::test]
    async fn build_backend_propagates_local_open_failure_instead_of_mock_fallback() {
        let dir = temp_dir("open-failure");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A file where the model directory should be forces create_dir_all
        // inside LocalHarrierBackend::open to fail.
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let settings = local_settings(&blocker);

        let result = build_backend(&dir, &settings, CancellationSource::new()).await;

        // The previous behavior silently fell back to a mock 640-dim backend;
        // the failure must now propagate so no fake vectors enter the index.
        assert!(
            matches!(result, Err(AppError::Configuration(_))),
            "open failure must propagate instead of falling back to a mock backend"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn status_snapshot_distinguishes_missing_files_from_standby() {
        let dir = temp_dir("standby");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let settings = local_settings(&dir);
        let backend = LocalHarrierBackend::open(
            settings.local.model.clone(),
            dir.clone(),
            &settings.local,
            CancellationSource::new(),
        )
        .await
        .unwrap();
        let manager =
            EmbeddingManager::from_backend_for_test(dir.clone(), settings, Arc::new(backend));

        // Files missing: not available at all.
        let missing = manager.status_snapshot();
        assert!(!missing.ok, "missing model files must not report ok");
        assert!(missing.message.contains("模型文件未就绪"));

        // Files present but weights not loaded: standby, not ready.
        for file in ["config.json", "tokenizer.json", "model.safetensors"] {
            std::fs::write(dir.join(file), b"placeholder").unwrap();
        }
        let standby = manager.status_snapshot();
        assert!(standby.ok, "files present must report available");
        assert!(
            standby.message.contains("按需加载"),
            "standby must be distinguishable from ready: {}",
            standby.message
        );
        assert!(
            !manager.is_ready(),
            "a standby backend must never be reported as ready"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
