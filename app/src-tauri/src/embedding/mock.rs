use async_trait::async_trait;
use sha2::{Digest, Sha256};

use super::{BackendIdentity, EmbeddingBackend, ensure_dimensions};
use crate::{
    error::Result,
    models::{EmbeddingBackendKind, EmbeddingHealth},
};

#[derive(Debug, Clone)]
pub struct MockEmbeddingBackend {
    kind: EmbeddingBackendKind,
    model_id: String,
    dimensions: usize,
}

impl MockEmbeddingBackend {
    pub fn new(kind: EmbeddingBackendKind, model_id: String, dimensions: usize) -> Self {
        Self {
            kind,
            model_id,
            dimensions: dimensions.max(8),
        }
    }

    fn embed_one(&self, text: &str, is_query: bool) -> Vec<f32> {
        let mut hasher = Sha256::new();
        if is_query {
            hasher.update(b"query:");
        } else {
            hasher.update(b"doc:");
        }
        hasher.update(text.as_bytes());
        let digest = hasher.finalize();
        let mut values = Vec::with_capacity(self.dimensions);
        for index in 0..self.dimensions {
            let byte = digest[index % digest.len()];
            let value = ((byte as f32) / 255.0) * 2.0 - 1.0;
            values.push(value);
        }
        let norm = values
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt()
            .max(1e-6);
        for value in &mut values {
            *value /= norm;
        }
        values
    }
}

#[async_trait]
impl EmbeddingBackend for MockEmbeddingBackend {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            backend: self.kind.clone(),
            backend_id: format!("{:?}", self.kind).to_ascii_lowercase(),
            model_id: self.model_id.clone(),
            dimensions: self.dimensions,
        }
    }

    async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let vectors = texts
            .iter()
            .map(|text| self.embed_one(text, false))
            .collect::<Vec<_>>();
        ensure_dimensions(&vectors, self.dimensions)?;
        Ok(vectors)
    }

    async fn embed_queries(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let vectors = texts
            .iter()
            .map(|text| self.embed_one(text, true))
            .collect::<Vec<_>>();
        ensure_dimensions(&vectors, self.dimensions)?;
        Ok(vectors)
    }

    async fn healthcheck(&self) -> Result<EmbeddingHealth> {
        Ok(EmbeddingHealth {
            ok: true,
            backend: self.kind.clone(),
            model_id: self.model_id.clone(),
            dimensions: Some(self.dimensions),
            message: "mock backend ready".into(),
        })
    }
}
