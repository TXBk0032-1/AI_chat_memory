use aws_sdk_s3::config::Credentials;
use aws_sigv4::{
    http_request::{
        PayloadChecksumKind, PercentEncodingMode, SignableBody, SignableRequest, SigningSettings,
        UriPathNormalizationMode, sign,
    },
    sign::v4,
};
use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::State,
    http::{HeaderName, Request, Response, StatusCode},
};
use chrono::NaiveDateTime;
use futures::{StreamExt, stream};
use quick_xml::{Reader, events::Event};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::{net::TcpListener, sync::Mutex, task::AbortHandle};

#[derive(Clone)]
struct Object {
    bytes: Vec<u8>,
    etag: String,
}

#[derive(Clone)]
struct RequestRecord {
    signed: bool,
    path_style: bool,
    virtual_hosted_style: bool,
}

struct ScriptedVaultGet {
    remaining_real_gets: usize,
    object: Object,
}

struct TestState {
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
    objects: Mutex<HashMap<String, Object>>,
    requests: Mutex<Vec<RequestRecord>>,
    list_requests: Mutex<usize>,
    bulk_delete: Mutex<bool>,
    fail_next_bulk_delete: Mutex<bool>,
    ignore_next_conditional_update: Mutex<bool>,
    fail_baseline_get_after: Mutex<Option<usize>>,
    fail_next_get_status: Mutex<Option<StatusCode>>,
    fail_next_get_error: Mutex<Option<(StatusCode, String)>>,
    stall_get_headers: Mutex<bool>,
    stall_get_body: Mutex<bool>,
    repeat_list_token: Mutex<bool>,
    scripted_vault_get: Mutex<Option<ScriptedVaultGet>>,
    reject_vault_conditional_updates: Mutex<bool>,
}

pub struct TestS3 {
    endpoint: String,
    state: Arc<TestState>,
    abort: AbortHandle,
}

impl TestS3 {
    pub async fn start(access_key_id: &str, session_token: Option<&str>) -> Self {
        let state = Arc::new(TestState {
            access_key_id: access_key_id.into(),
            secret_access_key: "secret-key".into(),
            session_token: session_token.map(str::to_owned),
            objects: Mutex::new(HashMap::new()),
            requests: Mutex::new(Vec::new()),
            list_requests: Mutex::new(0),
            bulk_delete: Mutex::new(false),
            fail_next_bulk_delete: Mutex::new(false),
            ignore_next_conditional_update: Mutex::new(false),
            fail_baseline_get_after: Mutex::new(None),
            fail_next_get_status: Mutex::new(None),
            fail_next_get_error: Mutex::new(None),
            stall_get_headers: Mutex::new(false),
            stall_get_body: Mutex::new(false),
            repeat_list_token: Mutex::new(false),
            scripted_vault_get: Mutex::new(None),
            reject_vault_conditional_updates: Mutex::new(false),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().fallback(handler).with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            endpoint: format!("http://{address}"),
            state,
            abort: task.abort_handle(),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
    pub fn virtual_hosted_endpoint(&self) -> String {
        self.endpoint.replacen("127.0.0.1", "localhost", 1)
    }
    pub async fn list_request_count(&self) -> usize {
        *self.state.list_requests.lock().await
    }
    pub async fn all_requests_signed(&self) -> bool {
        let requests = self.state.requests.lock().await;
        !requests.is_empty() && requests.iter().all(|request| request.signed)
    }
    pub async fn all_requests_use_path_style(&self) -> bool {
        self.state
            .requests
            .lock()
            .await
            .iter()
            .all(|request| request.path_style)
    }
    pub async fn saw_virtual_hosted_style(&self) -> bool {
        self.state
            .requests
            .lock()
            .await
            .iter()
            .any(|request| request.virtual_hosted_style)
    }
    pub async fn saw_bulk_delete(&self) -> bool {
        *self.state.bulk_delete.lock().await
    }
    pub async fn fail_next_bulk_delete(&self) {
        *self.state.fail_next_bulk_delete.lock().await = true;
    }
    pub async fn ignore_next_conditional_update(&self) {
        *self.state.ignore_next_conditional_update.lock().await = true;
    }
    pub async fn fail_baseline_get_after(&self, successful_gets: usize) {
        *self.state.fail_baseline_get_after.lock().await = Some(successful_gets);
    }

    pub async fn fail_next_get_with(&self, status: StatusCode) {
        *self.state.fail_next_get_status.lock().await = Some(status);
    }

    pub async fn fail_next_get_with_code(&self, status: StatusCode, code: &str) {
        *self.state.fail_next_get_error.lock().await = Some((status, code.to_owned()));
    }

    pub async fn clear_get_failure(&self) {
        *self.state.fail_next_get_status.lock().await = None;
    }

    pub async fn stall_get_headers(&self) {
        *self.state.stall_get_headers.lock().await = true;
    }

    pub async fn stall_get_body(&self) {
        *self.state.stall_get_body.lock().await = true;
    }

    pub async fn repeat_next_list_token(&self) {
        *self.state.repeat_list_token.lock().await = true;
    }

    pub async fn script_vault_change_after_gets(&self, successful_gets: usize, bytes: Vec<u8>) {
        let etag = format!("\"{}\"", hex::encode(Sha256::digest(&bytes)));
        *self.state.scripted_vault_get.lock().await = Some(ScriptedVaultGet {
            remaining_real_gets: successful_gets,
            object: Object { bytes, etag },
        });
        *self.state.reject_vault_conditional_updates.lock().await = true;
    }
}

impl Drop for TestS3 {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

async fn handler(State(state): State<Arc<TestState>>, request: Request<Body>) -> Response<Body> {
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, 128 * 1024 * 1024).await {
        Ok(body) => body,
        Err(_) => return s3_error(StatusCode::BAD_REQUEST, "InvalidRequest"),
    };
    let request = Request::from_parts(parts, Body::from(body.clone()));
    let signed = verify_signature(&request, &body, &state);
    let virtual_hosted_style = request
        .headers()
        .get("host")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| host.starts_with("archive."));
    let path_style = request.uri().path().starts_with("/archive");
    state.requests.lock().await.push(RequestRecord {
        signed,
        path_style,
        virtual_hosted_style,
    });
    if !signed {
        return s3_error(StatusCode::FORBIDDEN, "AccessDenied");
    }

    if request.method() == axum::http::Method::GET {
        let stall_headers = *state.stall_get_headers.lock().await;
        if stall_headers {
            std::future::pending::<()>().await;
            unreachable!("stalled S3 header fixture resumed unexpectedly");
        }
        // Keep status failures armed across SDK retries; this is how the existing
        // throttling fixture forces the complete logical operation to fail.
        let status_failure = *state.fail_next_get_status.lock().await;
        let code_failure = state.fail_next_get_error.lock().await.take();
        if let Some(status) = status_failure {
            return s3_error(status, "SlowDown");
        }
        if let Some((status, code)) = code_failure {
            return s3_error(status, &code);
        }
    }

    let method = request.method().clone();
    let uri = request.uri().clone();
    let query = uri.query().unwrap_or_default();
    let params = url::form_urlencoded::parse(query.as_bytes()).collect::<HashMap<_, _>>();
    let path_key = uri.path().trim_start_matches('/');
    let key = if virtual_hosted_style {
        path_key.to_owned()
    } else {
        path_key
            .strip_prefix("archive/")
            .unwrap_or_default()
            .to_owned()
    };
    if method == axum::http::Method::GET && params.contains_key("list-type") {
        *state.list_requests.lock().await += 1;
        return list_objects(&state, &params).await;
    }
    if method == axum::http::Method::POST && (query == "delete" || params.contains_key("delete")) {
        *state.bulk_delete.lock().await = true;
        let body = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
        let keys = delete_keys(&body);
        let fail = {
            let mut fail = state.fail_next_bulk_delete.lock().await;
            std::mem::take(&mut *fail)
        };
        if fail {
            let key = keys.first().map(String::as_str).unwrap_or("unknown");
            return xml(
                StatusCode::OK,
                format!(
                    "<DeleteResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Error><Key>{}</Key><Code>AccessDenied</Code><Message>fixture</Message></Error></DeleteResult>",
                    escape(key)
                ),
            );
        }
        for key in keys {
            state.objects.lock().await.remove(&key);
        }
        return xml(
            StatusCode::OK,
            "<DeleteResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"/>",
        );
    }
    if method == axum::http::Method::GET && key.contains("/devices/baseline/") {
        let mut remaining = state.fail_baseline_get_after.lock().await;
        if let Some(successful_gets) = remaining.as_mut() {
            if *successful_gets == 0 {
                *remaining = None;
                return s3_error(StatusCode::FORBIDDEN, "AccessDenied");
            }
            *successful_gets -= 1;
        }
    }
    if method == axum::http::Method::GET && key.ends_with("/v1/vault.json") {
        let scripted = {
            let mut scripted = state.scripted_vault_get.lock().await;
            match scripted.as_mut() {
                Some(script) if script.remaining_real_gets == 0 => {
                    scripted.take().map(|script| script.object)
                }
                Some(script) => {
                    script.remaining_real_gets -= 1;
                    None
                }
                None => None,
            }
        };
        if let Some(object) = scripted {
            *state.reject_vault_conditional_updates.lock().await = false;
            return Response::builder()
                .status(StatusCode::OK)
                .header("etag", object.etag)
                .body(Body::from(object.bytes))
                .unwrap();
        }
    }
    match method {
        axum::http::Method::PUT => {
            let headers = request.headers().clone();
            let bytes = to_bytes(request.into_body(), 128 * 1024 * 1024)
                .await
                .unwrap()
                .to_vec();
            if key.ends_with("/v1/vault.json")
                && headers.contains_key("if-match")
                && *state.reject_vault_conditional_updates.lock().await
            {
                return s3_error(StatusCode::PRECONDITION_FAILED, "PreconditionFailed");
            }
            let mut objects = state.objects.lock().await;
            let existing = objects.get(&key);
            if headers.get("if-none-match").and_then(|v| v.to_str().ok()) == Some("*")
                && existing.is_some()
            {
                return s3_error(StatusCode::PRECONDITION_FAILED, "PreconditionFailed");
            }
            if let Some(expected) = headers.get("if-match").and_then(|v| v.to_str().ok())
                && existing.map(|object| object.etag.as_str()) != Some(expected)
            {
                return s3_error(StatusCode::PRECONDITION_FAILED, "PreconditionFailed");
            }
            if headers.contains_key("if-match") {
                let mut ignore = state.ignore_next_conditional_update.lock().await;
                if std::mem::take(&mut *ignore) {
                    return Response::builder()
                        .status(StatusCode::OK)
                        .header(
                            "etag",
                            existing
                                .map(|object| object.etag.as_str())
                                .unwrap_or("\"missing\""),
                        )
                        .body(Body::empty())
                        .unwrap();
                }
            }
            let etag = format!("\"{}\"", hex::encode(Sha256::digest(&bytes)));
            objects.insert(
                key,
                Object {
                    bytes,
                    etag: etag.clone(),
                },
            );
            Response::builder()
                .status(StatusCode::OK)
                .header("etag", etag)
                .body(Body::empty())
                .unwrap()
        }
        axum::http::Method::GET if key == "oversized.bin" => {
            let bytes = vec![0u8; 128 * 1024 * 1024 + 1];
            Response::builder()
                .status(StatusCode::OK)
                .header("etag", "\"oversized\"")
                .body(Body::from(bytes))
                .unwrap()
        }
        axum::http::Method::GET => match state.objects.lock().await.get(&key).cloned() {
            Some(object) if *state.stall_get_body.lock().await => {
                let Object { bytes, etag } = object;
                let body = stream::once(async move {
                    Ok::<Bytes, std::convert::Infallible>(Bytes::from(bytes))
                })
                .chain(stream::pending());
                Response::builder()
                    .status(StatusCode::OK)
                    .header("etag", etag)
                    .body(Body::from_stream(body))
                    .unwrap()
            }
            Some(object) => Response::builder()
                .status(StatusCode::OK)
                .header("etag", object.etag)
                .body(Body::from(object.bytes))
                .unwrap(),
            None => s3_error(StatusCode::NOT_FOUND, "NoSuchKey"),
        },
        axum::http::Method::DELETE => {
            state.objects.lock().await.remove(&key);
            Response::builder()
                .status(StatusCode::NO_CONTENT)
                .body(Body::empty())
                .unwrap()
        }
        _ => s3_error(StatusCode::BAD_REQUEST, "InvalidRequest"),
    }
}

struct Authorization {
    credential_date: String,
    region: String,
    service: String,
    signed_headers: String,
}

fn verify_signature(request: &Request<Body>, body: &[u8], state: &TestState) -> bool {
    let Some(authorization) = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(parsed) = parse_authorization(authorization, &state.access_key_id) else {
        return false;
    };
    let token_matches = match (
        request.headers().get("x-amz-security-token"),
        state.session_token.as_deref(),
    ) {
        (None, None) => true,
        (Some(actual), Some(expected)) => actual.to_str().ok() == Some(expected),
        _ => false,
    };
    if !token_matches || parsed.service != "s3" {
        return false;
    }

    let Some(amz_date) = request
        .headers()
        .get("x-amz-date")
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    if amz_date.get(..8) != Some(parsed.credential_date.as_str()) {
        return false;
    }
    let Ok(timestamp) = NaiveDateTime::parse_from_str(amz_date, "%Y%m%dT%H%M%SZ") else {
        return false;
    };
    let Ok(seconds) = u64::try_from(timestamp.and_utc().timestamp()) else {
        return false;
    };
    let Some(signing_time) = SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
    else {
        return false;
    };

    let Some(payload_hash) = request
        .headers()
        .get("x-amz-content-sha256")
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let actual_hash = hex::encode(Sha256::digest(body));
    let signable_body = match payload_hash {
        "UNSIGNED-PAYLOAD" => SignableBody::UnsignedPayload,
        "STREAMING-UNSIGNED-PAYLOAD-TRAILER" => SignableBody::StreamingUnsignedPayloadTrailer,
        "STREAMING-AWS4-HMAC-SHA256-PAYLOAD-TRAILER" => SignableBody::StreamingSignedPayloadTrailer,
        value if value == actual_hash => SignableBody::Bytes(body),
        _ => return false,
    };

    let Some(host) = request
        .headers()
        .get("host")
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let uri = format!("http://{host}{}", request.uri());
    let mut headers = Vec::new();
    let mut previous = None;
    for name in parsed.signed_headers.split(';') {
        if name.is_empty() || previous.is_some_and(|previous| previous >= name) {
            return false;
        }
        previous = Some(name);
        let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) else {
            return false;
        };
        let mut found = false;
        for value in request.headers().get_all(&header_name) {
            let Ok(value) = value.to_str() else {
                return false;
            };
            headers.push((name.to_owned(), value.to_owned()));
            found = true;
        }
        if !found {
            return false;
        }
    }

    let identity = Credentials::new(
        &state.access_key_id,
        &state.secret_access_key,
        state.session_token.clone(),
        None,
        "test-s3-fixture",
    )
    .into();
    let mut settings = SigningSettings::default();
    settings.percent_encoding_mode = PercentEncodingMode::Single;
    settings.payload_checksum_kind = PayloadChecksumKind::XAmzSha256;
    settings.uri_path_normalization_mode = UriPathNormalizationMode::Disabled;
    let Ok(params) = v4::SigningParams::builder()
        .identity(&identity)
        .region(&parsed.region)
        .name(&parsed.service)
        .time(signing_time)
        .settings(settings)
        .build()
    else {
        return false;
    };
    let params = params.into();
    let Ok(signable_request) = SignableRequest::new(
        request.method().as_str(),
        uri,
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
        signable_body,
    ) else {
        return false;
    };
    let Ok(expected) = sign(signable_request, &params) else {
        return false;
    };
    expected
        .output()
        .headers()
        .find(|(name, _)| *name == "authorization")
        .is_some_and(|(_, value)| value == authorization)
}

fn parse_authorization(value: &str, expected_access_key_id: &str) -> Option<Authorization> {
    let value = value.strip_prefix("AWS4-HMAC-SHA256 ")?;
    let mut fields = value.split(", ");
    let credential = fields.next()?.strip_prefix("Credential=")?;
    let signed_headers = fields.next()?.strip_prefix("SignedHeaders=")?;
    let signature = fields.next()?.strip_prefix("Signature=")?;
    if fields.next().is_some()
        || signature.len() != 64
        || !signature.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    let mut scope = credential.split('/');
    if scope.next()? != expected_access_key_id {
        return None;
    }
    let credential_date = scope.next()?.to_owned();
    let region = scope.next()?.to_owned();
    let service = scope.next()?.to_owned();
    if scope.next()? != "aws4_request" || scope.next().is_some() {
        return None;
    }
    Some(Authorization {
        credential_date,
        region,
        service,
        signed_headers: signed_headers.to_owned(),
    })
}

async fn list_objects(
    state: &TestState,
    params: &HashMap<std::borrow::Cow<'_, str>, std::borrow::Cow<'_, str>>,
) -> Response<Body> {
    let prefix = params.get("prefix").map(|v| v.as_ref()).unwrap_or_default();
    let delimiter = params.get("delimiter").map(|v| v.as_ref());
    let start = params
        .get("continuation-token")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let objects = state.objects.lock().await;
    let mut entries = BTreeSet::new();
    for (key, object) in objects.iter().filter(|(key, _)| key.starts_with(prefix)) {
        let remainder = &key[prefix.len()..];
        if let Some(delimiter) = delimiter
            && let Some(index) = remainder.find(delimiter)
        {
            entries.insert((format!("{}{}", prefix, &remainder[..=index]), None));
        } else {
            entries.insert((key.clone(), Some((object.etag.clone(), object.bytes.len()))));
        }
    }
    let entries = entries.into_iter().collect::<Vec<_>>();
    let page = entries.iter().skip(start).take(2).collect::<Vec<_>>();
    let next = start + page.len();
    let truncated = next < entries.len();
    let mut body = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
    );
    body.push_str(&format!("<Name>archive</Name><Prefix>{}</Prefix><KeyCount>{}</KeyCount><MaxKeys>2</MaxKeys><IsTruncated>{}</IsTruncated>", escape(prefix), page.len(), truncated));
    if truncated {
        let repeat_token = *state.repeat_list_token.lock().await;
        let next_token = if repeat_token {
            params
                .get("continuation-token")
                .map(|value| value.to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "loop".to_owned())
        } else {
            next.to_string()
        };
        body.push_str(&format!(
            "<NextContinuationToken>{next_token}</NextContinuationToken>"
        ));
    }
    for (key, metadata) in page {
        match metadata {
            Some((etag, size)) => body.push_str(&format!("<Contents><Key>{}</Key><ETag>{}</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>", escape(key), escape(etag), size)),
            None => body.push_str(&format!("<CommonPrefixes><Prefix>{}</Prefix></CommonPrefixes>", escape(key))),
        }
    }
    body.push_str("</ListBucketResult>");
    xml(StatusCode::OK, body)
}

fn delete_keys(bytes: &[u8]) -> Vec<String> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    let mut in_key = false;
    let mut keys = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) if event.local_name().as_ref() == b"Key" => in_key = true,
            Ok(Event::Text(text)) if in_key => {
                keys.push(String::from_utf8_lossy(text.as_ref()).into_owned());
                in_key = false;
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    keys
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn xml(status: StatusCode, body: impl Into<Body>) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/xml")
        .body(body.into())
        .unwrap()
}
fn s3_error(status: StatusCode, code: &str) -> Response<Body> {
    xml(
        status,
        format!("<Error><Code>{code}</Code><Message>fixture</Message></Error>"),
    )
}
