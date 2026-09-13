use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;

use super::{BackendIdentity, CancellationToken, EmbeddingBackend, ensure_dimensions};
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

    async fn embed(
        &self,
        texts: &[String],
        _is_query: bool,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        match self.kind {
            EmbeddingBackendKind::Ollama => self.embed_ollama(texts, cancellation).await,
            EmbeddingBackendKind::LlamaCpp | EmbeddingBackendKind::OpenaiCompatible => {
                self.embed_openai(texts, cancellation).await
            }
            EmbeddingBackendKind::Local => unreachable!(),
        }
    }

    async fn embed_ollama(
        &self,
        texts: &[String],
        cancellation: Option<&CancellationToken>,
    ) -> Result<Vec<Vec<f32>>> {
        // Prefer the batch `/api/embed` endpoint: one round trip for the
        // whole batch instead of one per text, which makes full index rebuilds over
        // thousands of chunks dramatically faster. Fall back to the legacy
        // single-text `/api/embeddings` endpoint for servers that don't implement
        // the batch variant (older Ollama or compatible servers).
        if texts.len() > 1 {
            match self.embed_ollama_batch(texts, cancellation).await {
                Ok(vectors) => return Ok(vectors),
                Err(error @ AppError::Cancelled(_)) => return Err(error),
                Err(error) => {
                    tracing::warn!(%error, "ollama batch embed failed; falling back to per-text");
                }
            }
        }
        let mut vectors = Vec::with_capacity(texts.len());
        for text in texts {
            if cancellation.is_some_and(CancellationToken::is_cancelled) {
                return Err(AppError::Cancelled(CANCELLED_MESSAGE.into()));
            }
            let url = format!(
                "{}/api/embeddings",
                self.settings.base_url.trim_end_matches('/')
            );
            let request = self.client.post(url).json(&json!({
                "model": self.settings.model,
                "prompt": text,
            }));
            let response = cancellable(send_request(request), cancellation).await?;
            if !response.status().is_success() {
                let status = response.status();
                let body = error_body(response, cancellation).await?;
                return Err(AppError::Configuration(format!(
                    "ollama embeddings failed ({status}): {body}"
                )));
            }
            let payload: OllamaEmbeddingResponse =
                cancellable(parse_json(response), cancellation).await?;
            vectors.push(payload.embedding);
        }
        self.lock_or_infer_dimensions(&vectors)?;
        Ok(vectors)
    }

    async fn embed_ollama_batch(
        &self,
        texts: &[String],
        cancellation: Option<&CancellationToken>,
    ) -> Result<Vec<Vec<f32>>> {
        let url = format!("{}/api/embed", self.settings.base_url.trim_end_matches('/'));
        let request = self.client.post(url).json(&json!({
            "model": self.settings.model,
            "input": texts,
        }));
        let response = cancellable(send_request(request), cancellation).await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = error_body(response, cancellation).await?;
            return Err(AppError::Configuration(format!(
                "ollama embed batch failed ({status}): {body}"
            )));
        }
        let payload: OllamaEmbedBatchResponse =
            cancellable(parse_json(response), cancellation).await?;
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

    async fn embed_openai(
        &self,
        texts: &[String],
        cancellation: Option<&CancellationToken>,
    ) -> Result<Vec<Vec<f32>>> {
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
        let response = cancellable(send_request(request), cancellation).await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = error_body(response, cancellation).await?;
            return Err(AppError::Configuration(format!(
                "openai-compatible embeddings failed ({status}): {body}"
            )));
        }
        let payload: OpenAiEmbeddingResponse =
            cancellable(parse_json(response), cancellation).await?;
        let vectors = align_openai_embedding_vectors(payload.data, texts.len())?;
        self.lock_or_infer_dimensions(&vectors)?;
        Ok(vectors)
    }
}

const CANCELLED_MESSAGE: &str = "远程编码已取消";

/// Races `future` against `cancellation`. Without a token the future runs to
/// completion. `CancellationToken::cancelled()` only resolves on a real
/// cancel (it stays pending forever once every source is dropped), so it is
/// only ever awaited inside this `select!`, never on its own.
async fn cancellable<T>(
    future: impl std::future::Future<Output = Result<T>>,
    cancellation: Option<&CancellationToken>,
) -> Result<T> {
    let Some(token) = cancellation else {
        return future.await;
    };
    tokio::select! {
        biased;
        _ = token.cancelled() => Err(AppError::Cancelled(CANCELLED_MESSAGE.into())),
        result = future => result,
    }
}

async fn send_request(request: reqwest::RequestBuilder) -> Result<reqwest::Response> {
    request
        .send()
        .await
        .map_err(|error| AppError::Configuration(error.to_string()))
}

async fn parse_json<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<T> {
    response
        .json()
        .await
        .map_err(|error| AppError::Configuration(error.to_string()))
}

/// Reads the body of a non-2xx response for the error message. The read races
/// `cancellation` like every other network await: a cancelled task returns
/// `Err(Cancelled)` instead of waiting on a slow error body until the client
/// timeout. Body read failures degrade to an empty body, never to an error.
async fn error_body(
    response: reqwest::Response,
    cancellation: Option<&CancellationToken>,
) -> Result<String> {
    cancellable(
        async { Ok(response.text().await.unwrap_or_default()) },
        cancellation,
    )
    .await
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

    async fn embed_documents(
        &self,
        texts: &[String],
        cancellation: Option<&CancellationToken>,
    ) -> Result<Vec<Vec<f32>>> {
        self.embed(texts, false, cancellation).await
    }

    // Queries are interactive and never subject to background cancellation.
    async fn embed_queries(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed(texts, true, None).await
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
    use super::super::CancellationSource;
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
                "base_url={base_url} api_key_present={}",
                api_key.is_some()
            );
        }
    }

    #[test]
    fn openai_compatible_with_remote_http_and_api_key_still_constructs() {
        // A plaintext remote endpoint with an api key only warns; it must not hard-fail.
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

    /// Reads from `socket` until the end of the HTTP request headers so the
    /// client is known to be waiting on the response (never mid-connect).
    async fn read_request_headers(socket: &mut tokio::net::TcpStream) {
        use tokio::io::AsyncReadExt;
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
    }

    /// Spawns a fixture that accepts one connection, reads the request
    /// headers, writes `prefix` (possibly nothing) and then holds the socket
    /// open without ever finishing the response until the task is aborted.
    /// The returned receiver fires once the request headers have arrived.
    fn spawn_hanging_server(
        listener: tokio::net::TcpListener,
        prefix: &'static str,
    ) -> (
        tokio::task::JoinHandle<()>,
        tokio::sync::oneshot::Receiver<()>,
    ) {
        let (request_seen_tx, request_seen_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request_headers(&mut socket).await;
            if !prefix.is_empty() {
                socket.write_all(prefix.as_bytes()).await.unwrap();
                socket.flush().await.unwrap();
            }
            request_seen_tx.send(()).unwrap();
            // Hold the socket (never answer) until the test aborts this task;
            // the client side would otherwise stay pending up to its 60s timeout.
            std::future::pending::<()>().await;
            drop(socket);
        });
        (server, request_seen_rx)
    }

    /// Drives `request` (bounded by 250ms) concurrently with a canceller that
    /// waits for the fixture's "request received" signal, optionally lingers
    /// so the client has parsed whatever the fixture wrote, then cancels the
    /// source. Returns the request outcome and whether the signal arrived
    /// before the cancel was issued.
    async fn cancel_after_request_seen<F>(
        request: F,
        request_seen: tokio::sync::oneshot::Receiver<()>,
        linger: std::time::Duration,
        source: &CancellationSource,
    ) -> (
        std::result::Result<F::Output, tokio::time::error::Elapsed>,
        bool,
    )
    where
        F: std::future::Future,
    {
        let bounded = tokio::time::timeout(std::time::Duration::from_millis(250), request);
        let canceller = async {
            let seen = request_seen.await.is_ok();
            if !linger.is_zero() {
                tokio::time::sleep(linger).await;
            }
            source.cancel_current();
            seen
        };
        tokio::join!(bounded, canceller)
    }

    #[tokio::test]
    async fn document_request_returns_cancelled_before_http_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Accept the connection, read the request, but never answer: the
        // reqwest future is in flight and stays pending until its 60s timeout
        // unless cancellation interrupts it.
        let (server, request_seen) = spawn_hanging_server(listener, "");
        let backend = HttpEmbeddingBackend::openai_compatible(
            EmbeddingBackendKind::OpenaiCompatible,
            remote_settings(&format!("http://{addr}/v1"), None),
        )
        .unwrap();
        let source = CancellationSource::new();
        let token = source.token();
        let texts: [String; 1] = ["blocked".into()];
        let (result, cancelled_after_request_seen) = cancel_after_request_seen(
            backend.embed_documents(&texts, Some(&token)),
            request_seen,
            std::time::Duration::ZERO,
            &source,
        )
        .await;
        assert!(
            cancelled_after_request_seen,
            "cancel must be issued only after the server accepted and read the request"
        );
        let error = result
            .expect("cancel must beat 60 second timeout")
            .unwrap_err();
        assert!(matches!(error, AppError::Cancelled(_)), "got {error:?}");
        assert!(error.to_string().contains(CANCELLED_MESSAGE));
        server.abort();
    }

    #[tokio::test]
    async fn already_cancelled_token_short_circuits_without_connecting() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let backend = HttpEmbeddingBackend::openai_compatible(
            EmbeddingBackendKind::OpenaiCompatible,
            remote_settings(&format!("http://{addr}/v1"), None),
        )
        .unwrap();
        let source = CancellationSource::new();
        let token = source.token();
        source.cancel_current();
        let error = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            backend.embed_documents(&["never sent".into()], Some(&token)),
        )
        .await
        .expect("pre-cancelled request must return immediately")
        .unwrap_err();
        assert!(matches!(error, AppError::Cancelled(_)), "got {error:?}");
        // A pre-cancelled token must short-circuit before any connection is
        // attempted, so the listener never sees a client.
        let accepted =
            tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept()).await;
        assert!(
            accepted.is_err(),
            "no connection may be opened for a pre-cancelled token"
        );
    }

    #[tokio::test]
    async fn error_response_with_hanging_body_returns_cancelled() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Status and headers arrive so `send()` resolves with a 500, but the
        // announced body never does: reading the error body must still lose
        // the race against cancellation instead of waiting for the 60s timeout.
        let (server, request_seen) = spawn_hanging_server(
            listener,
            "HTTP/1.1 500 Internal Server Error\r\nContent-Type: text/plain\r\nContent-Length: 100\r\n\r\n",
        );
        let backend = HttpEmbeddingBackend::openai_compatible(
            EmbeddingBackendKind::OpenaiCompatible,
            remote_settings(&format!("http://{addr}/v1"), None),
        )
        .unwrap();
        let source = CancellationSource::new();
        let token = source.token();
        let texts: [String; 1] = ["hanging body".into()];
        // Linger so the client has certainly parsed the 500 status line and is
        // now blocked inside the body read, not still inside `send()`.
        let (result, cancelled_after_request_seen) = cancel_after_request_seen(
            backend.embed_documents(&texts, Some(&token)),
            request_seen,
            std::time::Duration::from_millis(50),
            &source,
        )
        .await;
        assert!(cancelled_after_request_seen);
        let error = result
            .expect("cancel must beat 60 second timeout while reading the error body")
            .unwrap_err();
        assert!(matches!(error, AppError::Cancelled(_)), "got {error:?}");
        server.abort();
    }

    #[tokio::test]
    async fn uncancelled_document_request_still_returns_vectors() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let (mut socket, _) = listener.accept().await.unwrap();
            // Drain the request headers before answering so the client never
            // sees a reset while it is still writing.
            read_request_headers(&mut socket).await;
            let body = r#"{"data":[{"index":0,"embedding":[0.1,0.2,0.3]}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        });
        let backend = HttpEmbeddingBackend::openai_compatible(
            EmbeddingBackendKind::OpenaiCompatible,
            remote_settings(&format!("http://{addr}/v1"), None),
        )
        .unwrap();
        let source = CancellationSource::new();
        let token = source.token();
        let vectors = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            backend.embed_documents(&["ok".into()], Some(&token)),
        )
        .await
        .expect("local fixture must answer promptly")
        .unwrap();
        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors[0], vec![0.1, 0.2, 0.3]);
        assert_eq!(backend.identity().dimensions, 3);
        server.await.unwrap();
    }
}
