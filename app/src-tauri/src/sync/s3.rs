use crate::{
    models::S3CloudSyncSettings,
    sync::backend::{
        CloudBackend, CloudError, CloudErrorKind, CloudResult, RemoteEntry, RemoteObject,
        RemotePath,
    },
};
use async_trait::async_trait;
use aws_sdk_s3::{
    Client,
    config::{Credentials, Region, timeout::TimeoutConfig},
    error::{ProvideErrorMetadata, SdkError},
    primitives::ByteStream,
    types::{Delete, ObjectIdentifier},
};
use std::{
    collections::{BTreeMap, HashSet},
    time::Duration,
};
use url::Url;
use uuid::Uuid;

const MAX_RESPONSE_BYTES: usize = 128 * 1024 * 1024;
const DELETE_BATCH_SIZE: usize = 1_000;
const MAX_LIST_PAGES: usize = 10_000;

#[derive(Clone, Copy)]
struct S3Timeouts {
    connect: Duration,
    read: Duration,
    attempt: Duration,
    operation: Duration,
    body_idle: Duration,
    body_total: Duration,
}

impl Default for S3Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            read: Duration::from_secs(30),
            attempt: Duration::from_secs(60),
            operation: Duration::from_secs(120),
            body_idle: Duration::from_secs(30),
            body_total: Duration::from_secs(10 * 60),
        }
    }
}

pub struct S3Backend {
    client: Client,
    bucket: String,
    prefix: String,
    timeouts: S3Timeouts,
    #[cfg(test)]
    force_path_style: bool,
}

impl S3Backend {
    pub fn new(
        settings: &S3CloudSyncSettings,
        access_key_id: &str,
        secret_access_key: &str,
        session_token: Option<&str>,
    ) -> CloudResult<Self> {
        Self::new_configured(
            settings,
            access_key_id,
            secret_access_key,
            session_token,
            S3Timeouts::default(),
            |builder| builder,
        )
    }

    fn new_configured(
        settings: &S3CloudSyncSettings,
        access_key_id: &str,
        secret_access_key: &str,
        session_token: Option<&str>,
        timeouts: S3Timeouts,
        configure: impl FnOnce(aws_sdk_s3::config::Builder) -> aws_sdk_s3::config::Builder,
    ) -> CloudResult<Self> {
        let region = settings.region.trim();
        let bucket = settings.bucket.trim();
        if region.is_empty() || bucket.is_empty() {
            return Err(protocol("S3 region and bucket are required"));
        }
        if access_key_id.is_empty() || secret_access_key.is_empty() {
            return Err(CloudError::new(
                CloudErrorKind::Auth,
                "S3 credentials are required",
            ));
        }
        let mut builder = aws_sdk_s3::Config::builder()
            .behavior_version_latest()
            .region(Region::new(region.to_owned()))
            .credentials_provider(Credentials::new(
                access_key_id,
                secret_access_key,
                session_token.map(str::to_owned),
                None,
                "ai-chat-memory",
            ))
            .timeout_config(
                TimeoutConfig::builder()
                    .connect_timeout(timeouts.connect)
                    .read_timeout(timeouts.read)
                    .operation_attempt_timeout(timeouts.attempt)
                    .operation_timeout(timeouts.operation)
                    .build(),
            )
            .force_path_style(settings.force_path_style);
        let endpoint = settings.endpoint_url.trim().trim_end_matches('/');
        if !endpoint.is_empty() {
            let parsed = Url::parse(endpoint).map_err(|_| protocol("invalid S3 endpoint URL"))?;
            if !matches!(parsed.scheme(), "http" | "https")
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                return Err(protocol("invalid S3 endpoint URL"));
            }
            builder = builder.endpoint_url(endpoint);
        }
        Ok(Self {
            client: Client::from_conf(configure(builder).build()),
            bucket: bucket.to_owned(),
            prefix: settings.prefix.trim().trim_matches('/').to_owned(),
            timeouts,
            #[cfg(test)]
            force_path_style: settings.force_path_style,
        })
    }

    #[cfg(test)]
    fn new_with_http_client(
        settings: &S3CloudSyncSettings,
        access_key_id: &str,
        secret_access_key: &str,
        session_token: Option<&str>,
        http_client: impl aws_sdk_s3::config::HttpClient + 'static,
    ) -> CloudResult<Self> {
        Self::new_configured(
            settings,
            access_key_id,
            secret_access_key,
            session_token,
            S3Timeouts::default(),
            |builder| builder.http_client(http_client),
        )
    }

    #[cfg(test)]
    fn new_with_timeouts(
        settings: &S3CloudSyncSettings,
        access_key_id: &str,
        secret_access_key: &str,
        session_token: Option<&str>,
        timeouts: S3Timeouts,
    ) -> CloudResult<Self> {
        Self::new_configured(
            settings,
            access_key_id,
            secret_access_key,
            session_token,
            timeouts,
            |builder| builder,
        )
    }

    #[cfg(test)]
    pub(crate) fn force_path_style(&self) -> bool {
        self.force_path_style
    }

    fn key(&self, path: &RemotePath) -> String {
        match (self.prefix.is_empty(), path.segments().is_empty()) {
            (true, true) => String::new(),
            (true, false) => path.display(),
            (false, true) => self.prefix.clone(),
            (false, false) => format!("{}/{}", self.prefix, path.display()),
        }
    }

    fn collection_prefix(&self, path: &RemotePath) -> String {
        let key = self.key(path);
        if key.is_empty() || key.ends_with('/') {
            key
        } else {
            format!("{key}/")
        }
    }

    async fn put_with_condition(
        &self,
        path: &RemotePath,
        bytes: &[u8],
        if_match: Option<&str>,
        if_none_match: Option<&str>,
    ) -> CloudResult<()> {
        let mut request = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(self.key(path))
            .body(ByteStream::from(bytes.to_vec()));
        if let Some(value) = if_match {
            request = request.if_match(value);
        }
        if let Some(value) = if_none_match {
            request = request.if_none_match(value);
        }
        request.send().await.map_err(map_sdk_error)?;
        Ok(())
    }

    async fn list_keys(&self, prefix: &str) -> CloudResult<Vec<String>> {
        let mut continuation = None;
        let mut keys = Vec::new();
        let mut seen_tokens = HashSet::new();
        let mut pages = 0;
        loop {
            pages += 1;
            if pages > MAX_LIST_PAGES {
                return Err(protocol("S3 listing exceeded page limit"));
            }
            let response = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix)
                .set_continuation_token(continuation)
                .send()
                .await
                .map_err(map_sdk_error)?;
            keys.extend(
                response
                    .contents()
                    .iter()
                    .filter_map(|object| object.key().map(str::to_owned)),
            );
            if response.is_truncated() != Some(true) {
                break;
            }
            let token = response
                .next_continuation_token()
                .map(str::to_owned)
                .filter(|token| !token.is_empty())
                .ok_or_else(|| protocol("S3 listing omitted continuation token"))?;
            if !seen_tokens.insert(token.clone()) {
                return Err(protocol("S3 listing repeated continuation token"));
            }
            continuation = Some(token);
        }
        Ok(keys)
    }

    async fn delete_keys(&self, keys: Vec<String>) -> CloudResult<()> {
        for chunk in keys.chunks(DELETE_BATCH_SIZE) {
            let objects = chunk
                .iter()
                .map(|key| {
                    ObjectIdentifier::builder()
                        .key(key)
                        .build()
                        .map_err(|_| protocol("invalid S3 delete key"))
                })
                .collect::<CloudResult<Vec<_>>>()?;
            let delete = Delete::builder()
                .set_objects(Some(objects))
                .quiet(true)
                .build()
                .map_err(|_| protocol("invalid S3 delete request"))?;
            let output = self
                .client
                .delete_objects()
                .bucket(&self.bucket)
                .delete(delete)
                .send()
                .await
                .map_err(map_sdk_error)?;
            if let Some(error) = output.errors().first() {
                let kind = match error.code() {
                    Some(
                        "AccessDenied"
                        | "AccountProblem"
                        | "AllAccessDisabled"
                        | "InvalidAccessKeyId"
                        | "SignatureDoesNotMatch",
                    ) => CloudErrorKind::Auth,
                    _ => CloudErrorKind::Protocol,
                };
                return Err(CloudError::new(
                    kind,
                    "S3 bulk delete reported object failures",
                ));
            }
        }
        Ok(())
    }
}

#[async_trait]
impl CloudBackend for S3Backend {
    async fn list_depth_one(&self, path: &RemotePath) -> CloudResult<Vec<RemoteEntry>> {
        let prefix = self.collection_prefix(path);
        let mut continuation = None;
        let mut entries = BTreeMap::new();
        let mut seen_tokens = HashSet::new();
        let mut pages = 0;
        loop {
            pages += 1;
            if pages > MAX_LIST_PAGES {
                return Err(protocol("S3 listing exceeded page limit"));
            }
            let response = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&prefix)
                .delimiter("/")
                .set_continuation_token(continuation)
                .send()
                .await
                .map_err(map_sdk_error)?;
            for object in response.contents() {
                let Some(key) = object.key() else { continue };
                let name = key.strip_prefix(&prefix).unwrap_or(key);
                if name.is_empty() || name.contains('/') {
                    continue;
                }
                entries.insert(
                    name.to_owned(),
                    RemoteEntry {
                        name: name.to_owned(),
                        is_collection: false,
                        etag: object.e_tag().map(str::to_owned),
                        size: object.size().and_then(|size| u64::try_from(size).ok()),
                    },
                );
            }
            for common in response.common_prefixes() {
                let Some(key) = common.prefix() else { continue };
                let name = key
                    .strip_prefix(&prefix)
                    .unwrap_or(key)
                    .trim_end_matches('/');
                if name.is_empty() || name.contains('/') {
                    continue;
                }
                entries.insert(
                    name.to_owned(),
                    RemoteEntry {
                        name: name.to_owned(),
                        is_collection: true,
                        etag: None,
                        size: None,
                    },
                );
            }
            if response.is_truncated() != Some(true) {
                break;
            }
            let token = response
                .next_continuation_token()
                .map(str::to_owned)
                .filter(|token| !token.is_empty())
                .ok_or_else(|| protocol("S3 listing omitted continuation token"))?;
            if !seen_tokens.insert(token.clone()) {
                return Err(protocol("S3 listing repeated continuation token"));
            }
            continuation = Some(token);
        }
        Ok(entries.into_values().collect())
    }

    async fn create_collection(&self, _path: &RemotePath) -> CloudResult<()> {
        Ok(())
    }

    async fn get(&self, path: &RemotePath) -> CloudResult<RemoteObject> {
        let output = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(self.key(path))
            .send()
            .await
            .map_err(map_sdk_error)?;
        if output
            .content_length()
            .is_some_and(|length| length < 0 || length as usize > MAX_RESPONSE_BYTES)
        {
            return Err(protocol("S3 response exceeds size limit"));
        }
        let etag = output.e_tag().map(str::to_owned);
        let mut body = output.body;
        let mut bytes = Vec::new();
        let started = tokio::time::Instant::now();
        loop {
            let Some(remaining) = self.timeouts.body_total.checked_sub(started.elapsed()) else {
                return Err(offline("S3 response body timed out"));
            };
            let wait = self.timeouts.body_idle.min(remaining);
            let next = tokio::time::timeout(wait, body.next())
                .await
                .map_err(|_| offline("S3 response body timed out"))?;
            let Some(chunk) = next else { break };
            let chunk = chunk.map_err(|_| offline("S3 response interrupted"))?;
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(protocol("S3 response exceeds size limit"));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(RemoteObject { bytes, etag })
    }

    async fn put_immutable(&self, path: &RemotePath, bytes: &[u8]) -> CloudResult<()> {
        self.put_if_absent(path, bytes).await
    }

    async fn put_if_match(&self, path: &RemotePath, bytes: &[u8], etag: &str) -> CloudResult<()> {
        self.put_with_condition(path, bytes, Some(etag), None).await
    }

    async fn put_if_absent(&self, path: &RemotePath, bytes: &[u8]) -> CloudResult<()> {
        self.put_with_condition(path, bytes, None, Some("*")).await
    }

    async fn delete(&self, path: &RemotePath) -> CloudResult<()> {
        let exact = self.key(path);
        let mut keys = self.list_keys(&self.collection_prefix(path)).await?;
        if !exact.is_empty() {
            keys.push(exact);
        }
        keys.sort();
        keys.dedup();
        self.delete_keys(keys).await
    }

    async fn test_capabilities(&self) -> CloudResult<()> {
        let probe = RemotePath::parse(&format!(".acm-probe-{}", Uuid::new_v4()))?;
        let object = probe.join("probe.bin")?;
        let result = async {
            self.create_collection(&probe).await?;
            self.put_if_absent(&object, b"one").await?;
            if !self
                .put_if_absent(&object, b"duplicate")
                .await
                .is_err_and(|error| error.is_precondition())
            {
                return Err(protocol("S3 ignored If-None-Match"));
            }
            let first = self.get(&object).await?;
            if first.bytes != b"one" {
                return Err(protocol("S3 probe content mismatch"));
            }
            let etag = first
                .etag
                .ok_or_else(|| protocol("S3 service did not return ETag"))?;
            if !self
                .put_if_match(&object, b"bad", "\"invalid\"")
                .await
                .is_err_and(|error| error.is_precondition())
            {
                return Err(protocol("S3 ignored invalid If-Match"));
            }
            self.put_if_match(&object, b"two", &etag).await?;
            let updated = self.get(&object).await?;
            if updated.bytes != b"two" {
                return Err(protocol("S3 ignored valid If-Match update"));
            }
            let listed = self.list_depth_one(&probe).await?;
            if !listed.iter().any(|entry| entry.name == "probe.bin") {
                return Err(protocol("S3 delimiter listing omitted probe"));
            }
            Ok(())
        }
        .await;
        let cleanup = self.delete(&probe).await;
        match (result, cleanup) {
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }
}

fn map_sdk_error<E: ProvideErrorMetadata>(error: SdkError<E>) -> CloudError {
    match &error {
        SdkError::TimeoutError(_) | SdkError::DispatchFailure(_) => {
            return offline("S3 endpoint is offline");
        }
        _ => {}
    }
    if let Some(kind) = error.code().and_then(map_service_error_code) {
        return CloudError::new(kind, "S3 service rejected the request");
    }
    match error
        .raw_response()
        .map(|response| response.status().as_u16())
    {
        Some(401 | 403) => CloudError::new(CloudErrorKind::Auth, "S3 authentication failed"),
        Some(404) => CloudError::new(CloudErrorKind::NotFound, "S3 object not found"),
        Some(409 | 412) => CloudError::new(CloudErrorKind::Precondition, "S3 precondition failed"),
        Some(429 | 500 | 502 | 503 | 504) => offline("S3 endpoint is temporarily unavailable"),
        _ => protocol("S3 protocol error"),
    }
}

fn map_service_error_code(code: &str) -> Option<CloudErrorKind> {
    match code {
        "AccessDenied"
        | "AccountProblem"
        | "AllAccessDisabled"
        | "AuthorizationHeaderMalformed"
        | "ExpiredToken"
        | "InvalidAccessKeyId"
        | "InvalidToken"
        | "SignatureDoesNotMatch" => Some(CloudErrorKind::Auth),
        "NoSuchBucket" | "NoSuchKey" | "NotFound" => Some(CloudErrorKind::NotFound),
        "ConditionalRequestConflict" | "PreconditionFailed" => Some(CloudErrorKind::Precondition),
        "RequestTimeout" | "ServiceUnavailable" | "SlowDown" => Some(CloudErrorKind::Offline),
        _ => None,
    }
}

fn protocol(message: &'static str) -> CloudError {
    CloudError::new(CloudErrorKind::Protocol, message)
}

fn offline(message: &'static str) -> CloudError {
    CloudError::new(CloudErrorKind::Offline, message)
}

#[cfg(test)]
mod tests {
    use super::{S3Backend, S3Timeouts};
    use crate::{
        models::S3CloudSyncSettings,
        sync::{
            backend::{CloudBackend, RemotePath},
            test_s3_server::TestS3,
        },
    };
    use aws_smithy_http_client::test_util::capture_request;
    use axum::http::StatusCode;
    use std::time::Duration;
    use url::Url;

    fn test_timeouts() -> S3Timeouts {
        S3Timeouts {
            connect: Duration::from_millis(100),
            read: Duration::from_millis(100),
            attempt: Duration::from_millis(150),
            operation: Duration::from_millis(300),
            body_idle: Duration::from_millis(100),
            body_total: Duration::from_millis(300),
        }
    }

    #[tokio::test]
    async fn s3_contract_covers_signing_pagination_conditions_and_recursive_cleanup() {
        let server = TestS3::start("AKID", Some("session-token")).await;
        let settings = S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "team".into(),
            force_path_style: true,
        };
        let backend =
            S3Backend::new(&settings, "AKID", "secret-key", Some("session-token")).unwrap();
        let root = RemotePath::parse("v1").unwrap();
        let head = RemotePath::parse("v1/head.json").unwrap();

        backend.create_collection(&root).await.unwrap();
        backend.put_if_absent(&head, b"one").await.unwrap();
        assert!(
            backend
                .put_if_absent(&head, b"duplicate")
                .await
                .unwrap_err()
                .is_precondition()
        );
        let first = backend.get(&head).await.unwrap();
        assert_eq!(first.bytes, b"one");
        assert!(
            backend
                .put_if_match(&head, b"bad", "\"wrong\"")
                .await
                .unwrap_err()
                .is_precondition()
        );
        backend
            .put_if_match(&head, b"two", first.etag.as_deref().unwrap())
            .await
            .unwrap();
        backend
            .put_immutable(&RemotePath::parse("v1/a.bin").unwrap(), b"a")
            .await
            .unwrap();
        backend
            .put_immutable(&RemotePath::parse("v1/nested/b.bin").unwrap(), b"b")
            .await
            .unwrap();

        let listed = backend.list_depth_one(&root).await.unwrap();
        assert!(
            listed
                .iter()
                .any(|entry| entry.name == "head.json" && !entry.is_collection)
        );
        assert!(
            listed
                .iter()
                .any(|entry| entry.name == "nested" && entry.is_collection)
        );
        assert!(
            server.list_request_count().await >= 2,
            "fixture paginates after two entries"
        );

        backend.delete(&root).await.unwrap();
        assert!(backend.list_depth_one(&root).await.unwrap().is_empty());
        assert!(server.all_requests_signed().await);
        assert!(server.all_requests_use_path_style().await);
        assert!(server.saw_bulk_delete().await);
    }

    #[tokio::test]
    async fn s3_list_keys_rejects_repeated_continuation_tokens() {
        let server = TestS3::start("AKID", None).await;
        let settings = S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "list-keys-loop".into(),
            force_path_style: true,
        };
        let backend = S3Backend::new(&settings, "AKID", "secret-key", None).unwrap();
        for name in ["a.bin", "b.bin", "c.bin"] {
            let path = format!("v1/{name}");
            backend
                .put_immutable(&RemotePath::parse(&path).unwrap(), b"data")
                .await
                .unwrap();
        }
        server.repeat_next_list_token().await;

        let error = backend.list_keys("list-keys-loop/v1/").await.unwrap_err();

        assert_eq!(error.kind(), "protocol");
    }

    #[tokio::test]
    async fn s3_list_depth_one_rejects_repeated_continuation_tokens() {
        let server = TestS3::start("AKID", None).await;
        let settings = S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "list-depth-loop".into(),
            force_path_style: true,
        };
        let backend = S3Backend::new(&settings, "AKID", "secret-key", None).unwrap();
        for name in ["a.bin", "b.bin", "c.bin"] {
            let path = format!("v1/{name}");
            backend
                .put_immutable(&RemotePath::parse(&path).unwrap(), b"data")
                .await
                .unwrap();
        }
        server.repeat_next_list_token().await;

        let error = backend
            .list_depth_one(&RemotePath::parse("v1").unwrap())
            .await
            .unwrap_err();

        assert_eq!(error.kind(), "protocol");
    }

    #[tokio::test]
    async fn s3_maps_auth_not_found_and_oversize_responses() {
        let server = TestS3::start("AKID", None).await;
        let settings = S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: String::new(),
            force_path_style: true,
        };
        let backend = S3Backend::new(&settings, "WRONG", "secret-key", None).unwrap();
        let missing = RemotePath::parse("missing.bin").unwrap();
        assert_eq!(backend.get(&missing).await.unwrap_err().kind(), "auth");

        let backend = S3Backend::new(&settings, "AKID", "wrong-secret", None).unwrap();
        assert_eq!(backend.get(&missing).await.unwrap_err().kind(), "auth");

        let backend = S3Backend::new(&settings, "AKID", "secret-key", None).unwrap();
        assert_eq!(backend.get(&missing).await.unwrap_err().kind(), "not_found");
        let oversized = RemotePath::parse("oversized.bin").unwrap();
        assert_eq!(
            backend.get(&oversized).await.unwrap_err().kind(),
            "protocol"
        );
    }

    #[tokio::test]
    async fn s3_maps_throttling_and_server_failures_to_offline() {
        let server = TestS3::start("AKID", None).await;
        let settings = S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: String::new(),
            force_path_style: true,
        };
        let backend = S3Backend::new(&settings, "AKID", "secret-key", None).unwrap();
        let path = RemotePath::parse("transient.bin").unwrap();

        for status in [
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            server.fail_next_get_with(status).await;
            assert_eq!(backend.get(&path).await.unwrap_err().kind(), "offline");
        }
    }

    #[tokio::test]
    async fn s3_times_out_when_the_server_never_sends_response_headers() {
        let server = TestS3::start("AKID", None).await;
        let settings = S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: String::new(),
            force_path_style: true,
        };
        let backend =
            S3Backend::new_with_timeouts(&settings, "AKID", "secret-key", None, test_timeouts())
                .unwrap();
        server.stall_get_headers().await;

        let error = tokio::time::timeout(
            Duration::from_secs(2),
            backend.get(&RemotePath::parse("stalled-headers.bin").unwrap()),
        )
        .await
        .expect("S3 header timeout must be bounded")
        .unwrap_err();

        assert_eq!(error.kind(), "offline");
    }

    #[tokio::test]
    async fn s3_times_out_when_a_response_body_stalls_after_its_first_chunk() {
        let server = TestS3::start("AKID", None).await;
        let settings = S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: String::new(),
            force_path_style: true,
        };
        let backend =
            S3Backend::new_with_timeouts(&settings, "AKID", "secret-key", None, test_timeouts())
                .unwrap();
        let path = RemotePath::parse("stalled-body.bin").unwrap();
        backend.put_if_absent(&path, b"first chunk").await.unwrap();
        server.stall_get_body().await;

        let error = tokio::time::timeout(Duration::from_secs(2), backend.get(&path))
            .await
            .expect("S3 body timeout must be bounded")
            .unwrap_err();

        assert_eq!(error.kind(), "offline");
    }

    #[tokio::test]
    async fn s3_maps_auth_service_codes_to_auth_even_when_http_status_is_bad_request() {
        let server = TestS3::start("AKID", None).await;
        let settings = S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: String::new(),
            force_path_style: true,
        };
        let backend = S3Backend::new(&settings, "AKID", "secret-key", None).unwrap();
        let path = RemotePath::parse("auth-code.bin").unwrap();

        for code in [
            "ExpiredToken",
            "InvalidToken",
            "AuthorizationHeaderMalformed",
        ] {
            server
                .fail_next_get_with_code(StatusCode::BAD_REQUEST, code)
                .await;
            assert_eq!(
                backend.get(&path).await.unwrap_err().kind(),
                "auth",
                "service error code {code} must take precedence over HTTP 400"
            );
        }
    }

    #[tokio::test]
    async fn s3_recursive_delete_reports_per_object_failures() {
        let server = TestS3::start("AKID", None).await;
        let settings = S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "delete-errors".into(),
            force_path_style: true,
        };
        let backend = S3Backend::new(&settings, "AKID", "secret-key", None).unwrap();
        let root = RemotePath::parse("v1").unwrap();
        backend
            .put_if_absent(&RemotePath::parse("v1/object.bin").unwrap(), b"data")
            .await
            .unwrap();
        server.fail_next_bulk_delete().await;

        let error = backend.delete(&root).await.unwrap_err();

        assert_eq!(error.kind(), "auth");
        assert!(!backend.list_depth_one(&root).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn s3_capability_probe_requires_recursive_cleanup_permission() {
        let server = TestS3::start("AKID", None).await;
        let settings = S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "probe-cleanup".into(),
            force_path_style: true,
        };
        let backend = S3Backend::new(&settings, "AKID", "secret-key", None).unwrap();
        server.fail_next_bulk_delete().await;

        assert_eq!(
            backend.test_capabilities().await.unwrap_err().kind(),
            "auth"
        );
    }

    #[tokio::test]
    async fn s3_capability_probe_reads_back_the_conditional_update() {
        let server = TestS3::start("AKID", None).await;
        let settings = S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "probe-readback".into(),
            force_path_style: true,
        };
        let backend = S3Backend::new(&settings, "AKID", "secret-key", None).unwrap();
        server.ignore_next_conditional_update().await;

        assert_eq!(
            backend.test_capabilities().await.unwrap_err().kind(),
            "protocol"
        );
    }

    #[test]
    fn s3_preserves_virtual_hosted_and_path_style_configuration() {
        let mut settings = S3CloudSyncSettings {
            endpoint_url: String::new(),
            region: "eu-west-1".into(),
            bucket: "archive".into(),
            prefix: String::new(),
            force_path_style: false,
        };
        assert!(
            !S3Backend::new(&settings, "AKID", "secret", None)
                .unwrap()
                .force_path_style()
        );
        settings.force_path_style = true;
        assert!(
            S3Backend::new(&settings, "AKID", "secret", None)
                .unwrap()
                .force_path_style()
        );
    }

    #[test]
    fn s3_rejects_endpoint_userinfo() {
        let settings = S3CloudSyncSettings {
            endpoint_url: "https://embedded:secret@s3.example.test".into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: String::new(),
            force_path_style: false,
        };

        let error = S3Backend::new(&settings, "AKID", "secret", None)
            .err()
            .expect("userinfo must be rejected");

        assert_eq!(error.kind(), "protocol");
    }

    #[tokio::test]
    async fn s3_virtual_hosted_style_places_bucket_in_the_host() {
        let settings = S3CloudSyncSettings {
            endpoint_url: "http://localhost:49152".into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: String::new(),
            force_path_style: false,
        };
        let (http_client, request) = capture_request(None);
        let backend =
            S3Backend::new_with_http_client(&settings, "AKID", "secret-key", None, http_client)
                .unwrap();
        let path = RemotePath::parse("virtual-hosted.bin").unwrap();

        backend.put_if_absent(&path, b"data").await.unwrap();
        let request = request.expect_request();
        let uri = Url::parse(request.uri()).unwrap();

        assert_eq!(uri.host_str(), Some("archive.localhost"));
        assert_eq!(uri.path(), "/virtual-hosted.bin");
    }
}
