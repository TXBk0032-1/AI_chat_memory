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
    dimensions: usize,
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
        Ok(Self {
            kind,
            dimensions: settings.dimensions.unwrap_or(640),
            settings,
            client,
            backend_id: backend_id.into(),
        })
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
        ensure_dimensions(&vectors, self.dimensions)?;
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
        if let Some(api_key) = self.settings.api_key.as_ref().filter(|value| !value.is_empty()) {
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
        items.sort_by_key(|item| item.index);
        let vectors = items.into_iter().map(|item| item.embedding).collect::<Vec<_>>();
        if vectors.len() != texts.len() {
            return Err(AppError::Configuration(
                "openai-compatible embeddings returned unexpected batch size".into(),
            ));
        }
        if let Some(dim) = self.settings.dimensions {
            ensure_dimensions(&vectors, dim)?;
        } else if let Some(first) = vectors.first() {
            // lock actual dimension for subsequent health/status reporting
            let _ = first;
        }
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
            dimensions: self.dimensions,
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
struct OpenAiEmbeddingResponse {
    data: Vec<OpenAiEmbeddingItem>,
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbeddingItem {
    index: usize,
    embedding: Vec<f32>,
}
