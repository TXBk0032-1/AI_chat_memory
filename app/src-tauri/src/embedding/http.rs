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
        if sends_credential_over_plaintext_http(&settings) {
            tracing::warn!(
                base_url = %settings.base_url,
                "embedding api_key is sent as a Bearer token over plaintext http to a non-local endpoint; use https to protect the credential"
            );
        }
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
        let vectors = align_openai_embedding_vectors(payload.data, texts.len())?;
        self.lock_or_infer_dimensions(&vectors)?;
        Ok(vectors)
    }
}

/// True when a configured api_key would travel over plaintext http to a
/// non-loopback endpoint. Local http endpoints (127.0.0.1/localhost/[::1]) are
/// common for self-hosted runners and stay silent; remote ones only produce a
/// warning, never a hard failure.
fn sends_credential_over_plaintext_http(settings: &RemoteEmbeddingSettings) -> bool {
    let has_api_key = settings
        .api_key
        .as_ref()
        .is_some_and(|value| !value.trim().is_empty());
    if !has_api_key {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(settings.base_url.trim()) else {
        return false;
    };
    if url.scheme() != "http" {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    !(host.eq_ignore_ascii_case("localhost")
        || host.starts_with("127.")
        || host == "::1"
        || host == "::ffff:127.0.0.1")
}

/// Aligns `data` items from an OpenAI-compatible embeddings response with the
/// request order. When every item carries an index the vectors are sorted into
/// request order; otherwise the server's returned order is trusted as-is after
/// a warning, and a count mismatch is always an error instead of a silent
/// misalignment.
fn align_openai_embedding_vectors(
    items: Vec<OpenAiEmbeddingItem>,
    expected: usize,
) -> Result<Vec<Vec<f32>>> {
    if items.len() != expected {
        return Err(AppError::Configuration(format!(
            "openai-compatible embeddings returned {} vectors for {expected} inputs",
            items.len()
        )));
    }
    if items.iter().all(|item| item.index.is_some()) {
        let mut items = items;
        items.sort_by_key(|item| item.index.unwrap_or(0));
        return Ok(items.into_iter().map(|item| item.embedding).collect());
    }
    tracing::warn!(
        expected,
        "openai-compatible embedding response missing index fields; trusting server order"
    );
    Ok(items.into_iter().map(|item| item.embedding).collect())
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

    fn remote_settings(base_url: &str, api_key: Option<&str>) -> RemoteEmbeddingSettings {
        RemoteEmbeddingSettings {
            base_url: base_url.into(),
            api_key: api_key.map(str::to_owned),
            model: "test-model".into(),
            dimensions: None,
        }
    }

    #[test]
    fn plaintext_http_credential_warning_targets_only_remote_endpoints() {
        let cases: &[(&str, Option<&str>, bool)] = &[
            ("http://api.example.com/v1", Some("secret"), true),
            ("http://192.168.1.10:8080/v1", Some("secret"), true),
            ("http://api.example.com/v1", Some("  "), false),
            ("http://api.example.com/v1", None, false),
            ("https://api.example.com/v1", Some("secret"), false),
            ("http://127.0.0.1:8080/v1", Some("secret"), false),
            ("http://localhost:1234/v1", Some("secret"), false),
            ("http://[::1]:8080/v1", Some("secret"), false),
            ("not a url", Some("secret"), false),
        ];
        for (base_url, api_key, expected) in cases {
            assert_eq!(
                sends_credential_over_plaintext_http(&remote_settings(base_url, *api_key)),
                *expected,
                "base_url={base_url} api_key={api_key:?}"
            );
        }
    }

    #[test]
    fn openai_compatible_with_remote_http_and_api_key_still_constructs() {
        // CFG-5 only warns; a plaintext remote endpoint must not hard-fail.
        let backend = HttpEmbeddingBackend::openai_compatible(
            EmbeddingBackendKind::OpenaiCompatible,
            remote_settings("http://api.example.com/v1", Some("secret")),
        );
        assert!(backend.is_ok());
    }

    fn item(index: Option<usize>, embedding: Vec<f32>) -> OpenAiEmbeddingItem {
        OpenAiEmbeddingItem { index, embedding }
    }

    #[test]
    fn aligns_indexed_response_items_into_request_order() {
        let items = vec![
            item(Some(2), vec![3.0, 3.0]),
            item(Some(0), vec![1.0, 1.0]),
            item(Some(1), vec![2.0, 2.0]),
        ];
        let vectors = align_openai_embedding_vectors(items, 3).unwrap();
        assert_eq!(vectors[0], vec![1.0, 1.0]);
        assert_eq!(vectors[1], vec![2.0, 2.0]);
        assert_eq!(vectors[2], vec![3.0, 3.0]);
    }

    #[test]
    fn missing_index_keeps_server_order_and_mismatch_is_an_error() {
        // Partial index coverage: trust the returned order instead of a
        // partial sort that could scramble the batch.
        let items = vec![
            item(Some(0), vec![1.0]),
            item(None, vec![2.0]),
            item(None, vec![3.0]),
        ];
        let vectors = align_openai_embedding_vectors(items, 3).unwrap();
        assert_eq!(vectors[0], vec![1.0]);
        assert_eq!(vectors[1], vec![2.0]);
        assert_eq!(vectors[2], vec![3.0]);

        // Count mismatches must error rather than silently misalign.
        let items = vec![item(Some(0), vec![1.0]), item(Some(1), vec![2.0])];
        let error = align_openai_embedding_vectors(items, 3).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("returned 2 vectors for 3 inputs")
        );
    }
}
