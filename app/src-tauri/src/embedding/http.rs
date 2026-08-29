use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;

use super::{BackendIdentity, EmbeddingBackend, ensure_dimensions};
use crate::{
    error::{AppError, Result},
    models::{EmbeddingBackendKind, EmbeddingHealth, RemoteEmbeddingSettings},
};

#[derive(Debug, Clone)]
pub struct HttpEmbeddingBackend {
    kind: EmbeddingBackendKind,
    settings: RemoteEmbeddingSettings,
    client: Client,
    backend_id: String,
    dimensions: std::sync::Arc<std::sync::RwLock<usize>>,
}

impl HttpEmbeddingBackend {
    pub fn ollama(settings: RemoteEmbeddingSettings) -> Result<Self> {
        Self::new(EmbeddingBackendKind::Ollama, settings, "ollama")
    }

    pub fn openai_compatible(
        kind: EmbeddingBackendKind,
        settings: RemoteEmbeddingSettings,
    ) -> Result<Self> {
        let backend_id = match kind {
            EmbeddingBackendKind::LlamaCpp => "llama_cpp",
            EmbeddingBackendKind::OpenaiCompatible => "openai_compatible",
            EmbeddingBackendKind::Ollama | EmbeddingBackendKind::Local => {
                return Err(AppError::Configuration(
                    "invalid openai-compatible backend kind".into(),
                ));
            }
        };
        Self::new(kind, settings, backend_id)
    }

    fn new(
        kind: EmbeddingBackendKind,
        settings: RemoteEmbeddingSettings,
        backend_id: &str,
    ) -> Result<Self> {
        if settings.base_url.trim().is_empty() || settings.model.trim().is_empty() {
            return Err(AppError::Configuration(
                "embedding backend base_url and model are required".into(),
            ));
        }
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|error| AppError::Configuration(error.to_string()))?;
        let configured = settings.dimensions.unwrap_or(0);
        Ok(Self {
            kind,
            dimensions: std::sync::Arc::new(std::sync::RwLock::new(if configured > 0 {
                configured
            } else {
                512
            })),
            settings,
            client,
            backend_id: backend_id.into(),
        })
    }

    fn current_dimensions(&self) -> usize {
        *self.dimensions.read().unwrap_or_else(|e| e.into_inner())
    }

    fn note_dimensions(&self, dim: usize) {
        if dim == 0 {
            return;
        }
        if let Ok(mut guard) = self.dimensions.write() {
            *guard = dim;
        }
    }

    fn lock_or_infer_dimensions(&self, vectors: &[Vec<f32>]) -> Result<usize> {
        if let Some(expected) = self.settings.dimensions.filter(|value| *value > 0) {
            ensure_dimensions(vectors, expected)?;
            self.note_dimensions(expected);
            return Ok(expected);
        }
        let Some(first) = vectors.first() else {
            return Ok(self.current_dimensions());
        };
        let dim = first.len();
        if dim == 0 {
            return Err(AppError::Configuration("embedding vector is empty".into()));
        }
        for vector in vectors {
            if vector.len() != dim {
                return Err(AppError::Configuration(format!(
                    "embedding dimension mismatch within batch: expected {dim}, got {}",
                    vector.len()
                )));
            }
        }
        self.note_dimensions(dim);
        Ok(dim)
    }

    async fn embed(&self, texts: &[String], _is_query: bool) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        match self.kind {
            EmbeddingBackendKind::Ollama => self.embed_ollama(texts).await,
            EmbeddingBackendKind::LlamaCpp | EmbeddingBackendKind::OpenaiCompatible => {
                self.embed_openai(texts).await
            }
            EmbeddingBackendKind::Local => unreachable!(),
        }
    }

    async fn embed_ollama(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        // Prefer the batch `/api/embed` endpoint: one round trip for the
        // whole batch instead of one per text, which makes full index rebuilds over
        // thousands of chunks dramatically faster. Fall back to the legacy
        // single-text `/api/embeddings` endpoint for servers that don't implement
        // the batch variant (older Ollama or compatible servers).
        if texts.len() > 1 {
            match self.embed_ollama_batch(texts).await {
                Ok(vectors) => return Ok(vectors),
                Err(error) => {
                    tracing::warn!(%error, "ollama batch embed failed; falling back to per-text");
                }
            }
        }
        let mut vectors = Vec::with_capacity(texts.len());
        for text in texts {
            let url = format!(
                "{}/api/embeddings",
                self.settings.base_url.trim_end_matches('/')
            );
            let response = self
                .client
                .post(url)
                .json(&json!({
                    "model": self.settings.model,
                    "prompt": text,
                }))
                .send()
                .await
                .map_err(|error| AppError::Configuration(error.to_string()))?;
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(AppError::Configuration(format!(
                    "ollama embeddings failed ({status}): {body}"
                )));
            }
            let payload: OllamaEmbeddingResponse = response
                .json()
                .await
                .map_err(|error| AppError::Configuration(error.to_string()))?;
            vectors.push(payload.embedding);
        }
        self.lock_or_infer_dimensions(&vectors)?;
        Ok(vectors)
    }

    async fn embed_ollama_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let url = format!("{}/api/embed", self.settings.base_url.trim_end_matches('/'));
        let response = self
            .client
            .post(url)
            .json(&json!({
                "model": self.settings.model,
                "input": texts,
            }))
            .send()
            .await
            .map_err(|error| AppError::Configuration(error.to_string()))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AppError::Configuration(format!(
                "ollama embed batch failed ({status}): {body}"
            )));
        }
        let payload: OllamaEmbedBatchResponse = response
            .json()
            .await
            .map_err(|error| AppError::Configuration(error.to_string()))?;
        let vectors = payload.embeddings;
        if vectors.len() != texts.len() {
            return Err(AppError::Configuration(format!(
                "ollama embed batch returned {} vectors for {} inputs",
                vectors.len(),
                texts.len()
            )));
        }
        self.lock_or_infer_dimensions(&vectors)?;
        Ok(vectors)
    }

    async fn embed_openai(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let url = format!(
            "{}/embeddings",
            self.settings.base_url.trim_end_matches('/')
        );
        let mut request = self.client.post(url).json(&json!({
            "model": self.settings.model,
            "input": texts,
        }));
        if let Some(api_key) = self
            .settings
            .api_key
            .as_ref()
            .filter(|value| !value.is_empty())
        {
            request = request.bearer_auth(api_key);
        }
        let response = request
            .send()
            .await
            .map_err(|error| AppError::Configuration(error.to_string()))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AppError::Configuration(format!(
                "openai-compatible embeddings failed ({status}): {body}"
            )));
        }
        let payload: OpenAiEmbeddingResponse = response
            .json()
            .await
            .map_err(|error| AppError::Configuration(error.to_string()))?;
        let mut items = payload.data;
        if items.iter().all(|item| item.index.is_some()) {
            items.sort_by_key(|item| item.index.unwrap_or(0));
        }
        let vectors = items
            .into_iter()
            .map(|item| item.embedding)
            .collect::<Vec<_>>();
        if vectors.len() != texts.len() {
            return Err(AppError::Configuration(
                "openai-compatible embeddings returned unexpected batch size".into(),
            ));
        }
        self.lock_or_infer_dimensions(&vectors)?;
        Ok(vectors)
    }
}

#[async_trait]
impl EmbeddingBackend for HttpEmbeddingBackend {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            backend: self.kind.clone(),
            backend_id: self.backend_id.clone(),
            model_id: self.settings.model.clone(),
            dimensions: self.current_dimensions(),
        }
    }

    async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed(texts, false).await
    }

    async fn embed_queries(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed(texts, true).await
    }

    async fn healthcheck(&self) -> Result<EmbeddingHealth> {
        let sample = self.embed_queries(&["healthcheck".into()]).await?;
        let dimensions = sample.first().map(Vec::len);
        Ok(EmbeddingHealth {
            ok: dimensions.is_some(),
            backend: self.kind.clone(),
            model_id: self.settings.model.clone(),
            dimensions,
            message: if dimensions.is_some() {
                "ok".into()
            } else {
                "empty embedding response".into()
            },
        })
    }
}

#[derive(Debug, Deserialize)]
struct OllamaEmbeddingResponse {
    embedding: Vec<f32>,
}

#[derive(Debug, Deserialize)]
struct OllamaEmbedBatchResponse {
    embeddings: Vec<Vec<f32>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbeddingResponse {
    data: Vec<OpenAiEmbeddingItem>,
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbeddingItem {
    #[serde(default)]
    index: Option<usize>,
    embedding: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(dimensions: Option<usize>) -> RemoteEmbeddingSettings {
        RemoteEmbeddingSettings {
            base_url: "http://127.0.0.1:1".into(),
            api_key: None,
            model: "test-model".into(),
            dimensions,
        }
    }

    #[test]
    fn infers_non_640_dimensions_from_response() {
        let backend = HttpEmbeddingBackend::openai_compatible(
            EmbeddingBackendKind::OpenaiCompatible,
            settings(None),
        )
        .unwrap();
        backend
            .lock_or_infer_dimensions(&[vec![0.0; 384], vec![1.0; 384]])
            .unwrap();
        assert_eq!(backend.identity().dimensions, 384);
    }

    #[test]
    fn configured_dimensions_reject_mismatch() {
        let backend = HttpEmbeddingBackend::openai_compatible(
            EmbeddingBackendKind::OpenaiCompatible,
            settings(Some(512)),
        )
        .unwrap();
        let error = backend
            .lock_or_infer_dimensions(&[vec![0.0; 384]])
            .unwrap_err();
        assert!(error.to_string().contains("expected 512, got 384"));
    }
}
