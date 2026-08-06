use crate::sync::webdav::WebDavBackend;
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    http::{Request, Response, StatusCode},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc};
use tokio::{net::TcpListener, sync::Mutex, task::AbortHandle};

#[derive(Clone)]
struct Node {
    collection: bool,
    bytes: Vec<u8>,
    etag: Option<String>,
}

struct TestState {
    auth: String,
    nodes: Mutex<HashMap<String, Node>>,
    methods: Mutex<Vec<String>>,
}

pub struct TestWebDav {
    base_url: String,
    state: Arc<TestState>,
    abort: AbortHandle,
}

impl TestWebDav {
    pub async fn start(username: &str, password: &str) -> Self {
        let state = Arc::new(TestState {
            auth: format!(
                "Basic {}",
                STANDARD.encode(format!("{username}:{password}"))
            ),
            nodes: Mutex::new(HashMap::from([(
                String::new(),
                Node {
                    collection: true,
                    bytes: Vec::new(),
                    etag: None,
                },
            )])),
            methods: Mutex::new(Vec::new()),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().fallback(handler).with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            base_url: format!("http://{address}/"),
            state,
            abort: task.abort_handle(),
        }
    }

    pub fn client(
        &self,
        username: &str,
        password: &str,
    ) -> crate::sync::backend::CloudResult<WebDavBackend> {
        WebDavBackend::new(&self.base_url, username, password)
    }

    pub fn endpoint(&self) -> &str {
        &self.base_url
    }

    pub async fn methods(&self) -> Vec<String> {
        self.state.methods.lock().await.clone()
    }
}

impl Drop for TestWebDav {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

async fn handler(State(state): State<Arc<TestState>>, request: Request<Body>) -> Response<Body> {
    let method = request.method().as_str().to_owned();
    state.methods.lock().await.push(method.clone());
    let authenticated = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == state.auth);
    if !authenticated {
        return response(StatusCode::UNAUTHORIZED, "authentication required");
    }
    let path = request.uri().path().trim_matches('/').to_owned();
    let headers = request.headers().clone();
    let body = request.into_body();
    match method.as_str() {
        "MKCOL" => {
            let parent = path.rsplit_once('/').map(|value| value.0).unwrap_or("");
            let mut nodes = state.nodes.lock().await;
            if nodes.contains_key(&path) {
                return response(StatusCode::METHOD_NOT_ALLOWED, "exists");
            }
            if !nodes.get(parent).is_some_and(|node| node.collection) {
                return response(StatusCode::CONFLICT, "parent missing");
            }
            nodes.insert(
                path,
                Node {
                    collection: true,
                    bytes: Vec::new(),
                    etag: None,
                },
            );
            response(StatusCode::CREATED, "")
        }
        "PUT" => {
            let bytes = match to_bytes(body, 128 * 1024 * 1024).await {
                Ok(bytes) => bytes.to_vec(),
                Err(_) => return response(StatusCode::PAYLOAD_TOO_LARGE, "too large"),
            };
            let mut nodes = state.nodes.lock().await;
            let existing = nodes.get(&path);
            if headers
                .get("if-none-match")
                .and_then(|value| value.to_str().ok())
                == Some("*")
                && existing.is_some()
            {
                return response(StatusCode::PRECONDITION_FAILED, "exists");
            }
            if let Some(expected) = headers
                .get("if-match")
                .and_then(|value| value.to_str().ok())
                && existing.and_then(|node| node.etag.as_deref()) != Some(expected)
            {
                return response(StatusCode::PRECONDITION_FAILED, "etag mismatch");
            }
            let etag = format!("\"{}\"", hex::encode(Sha256::digest(&bytes)));
            let status = if existing.is_some() {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::CREATED
            };
            nodes.insert(
                path,
                Node {
                    collection: false,
                    bytes,
                    etag: Some(etag.clone()),
                },
            );
            Response::builder()
                .status(status)
                .header("etag", etag)
                .body(Body::empty())
                .unwrap()
        }
        "GET" => {
            let nodes = state.nodes.lock().await;
            let Some(node) = nodes.get(&path) else {
                return response(StatusCode::NOT_FOUND, "missing");
            };
            let mut builder = Response::builder().status(StatusCode::OK);
            if let Some(etag) = &node.etag {
                builder = builder.header("etag", etag);
            }
            builder.body(Body::from(node.bytes.clone())).unwrap()
        }
        "DELETE" => {
            let mut nodes = state.nodes.lock().await;
            let prefix = format!("{path}/");
            nodes.retain(|key, _| key != &path && !key.starts_with(&prefix));
            response(StatusCode::NO_CONTENT, "")
        }
        "PROPFIND" => {
            let nodes = state.nodes.lock().await;
            if !nodes.contains_key(&path) {
                return response(StatusCode::NOT_FOUND, "missing");
            }
            let prefix = if path.is_empty() {
                String::new()
            } else {
                format!("{path}/")
            };
            let mut xml = String::from("<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\">");
            let mut keys = nodes
                .keys()
                .filter(|key| {
                    key.starts_with(&prefix) && *key != &path && !key[prefix.len()..].contains('/')
                })
                .cloned()
                .collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                let node = &nodes[&key];
                xml.push_str(&format!(
                    "<d:response><d:href>/{key}</d:href><d:propstat><d:prop>"
                ));
                if let Some(etag) = &node.etag {
                    xml.push_str(&format!("<d:getetag>{etag}</d:getetag>"));
                }
                xml.push_str(&format!(
                    "<d:getcontentlength>{}</d:getcontentlength><d:resourcetype>",
                    node.bytes.len()
                ));
                if node.collection {
                    xml.push_str("<d:collection/>");
                }
                xml.push_str("</d:resourcetype></d:prop></d:propstat></d:response>");
            }
            xml.push_str("</d:multistatus>");
            response(StatusCode::MULTI_STATUS, xml)
        }
        _ => response(StatusCode::METHOD_NOT_ALLOWED, "unsupported"),
    }
}

fn response(status: StatusCode, body: impl Into<Body>) -> Response<Body> {
    Response::builder()
        .status(status)
        .body(body.into())
        .unwrap()
}
