use crate::sync::backend::{
    CloudBackend, CloudError, CloudErrorKind, CloudResult, RemoteEntry, RemoteObject, RemotePath,
};
use async_trait::async_trait;
use futures::StreamExt;
use quick_xml::{Reader, events::Event};
use reqwest::{Client, Method, Response, StatusCode, redirect::Policy};
use std::time::Duration;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

const PROPFIND_BODY: &str = "<?xml version=\"1.0\"?><propfind xmlns=\"DAV:\"><prop><getetag/><getcontentlength/><resourcetype/></prop></propfind>";

const MAX_RESPONSE_BYTES: usize = 128 * 1024 * 1024;
// 幂等请求针对网关瞬断的重试预算。链路上的代理/TUN 软件在重负载下会把成功
// 应答改写成 502（本机 FlClash TUN 实测 207 被换成 502），真实部署的 WebDAV
// 网关也会瞬断；单次重试足以把偶发抖动压到可忽略。带条件的写操作不重试，
// 否则首次写实际成功而应答丢失时，重放会得到 412 造成假冲突。
const MAX_IDEMPOTENT_ATTEMPTS: usize = 2;
const IDEMPOTENT_RETRY_DELAY: Duration = Duration::from_millis(100);

pub struct WebDavBackend {
    base_url: Url,
    username: String,
    password: Zeroizing<String>,
    client: Client,
}

impl WebDavBackend {
    pub fn new(base_url: &str, username: &str, password: &str) -> CloudResult<Self> {
        Self::new_with_timeouts(
            base_url,
            username,
            password,
            Duration::from_secs(30),
            Duration::from_secs(120),
        )
    }

    pub fn new_with_timeouts(
        base_url: &str,
        username: &str,
        password: &str,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> CloudResult<Self> {
        let mut base_url = Url::parse(base_url).map_err(|_| protocol("invalid WebDAV URL"))?;
        if !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
            || !matches!(base_url.scheme(), "http" | "https")
        {
            return Err(protocol("WebDAV URL contains unsupported components"));
        }
        if !base_url.path().ends_with('/') {
            base_url
                .path_segments_mut()
                .map_err(|_| protocol("WebDAV URL cannot be a base"))?
                .push("");
        }
        let client = Client::builder()
            .redirect(Policy::none())
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .build()
            .map_err(|_| protocol("failed to construct WebDAV client"))?;
        Ok(Self {
            base_url,
            username: username.to_owned(),
            password: Zeroizing::new(password.to_owned()),
            client,
        })
    }

    fn url(&self, path: &RemotePath) -> CloudResult<Url> {
        let mut url = self.base_url.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| protocol("WebDAV URL cannot contain path segments"))?;
            for segment in path.segments() {
                segments.push(segment);
            }
        }
        Ok(url)
    }

    fn request(&self, method: Method, path: &RemotePath) -> CloudResult<reqwest::RequestBuilder> {
        Ok(self
            .client
            .request(method, self.url(path)?)
            .basic_auth(&self.username, Some(self.password.as_str())))
    }

    async fn expect_success(&self, response: Response) -> CloudResult<Response> {
        if response.status().is_success() || response.status() == StatusCode::MULTI_STATUS {
            Ok(response)
        } else {
            Err(map_status(response.status()))
        }
    }

    async fn read_limited(response: Response) -> CloudResult<Vec<u8>> {
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        loop {
            let next_chunk = tokio::time::timeout(Duration::from_secs(30), stream.next())
                .await
                .map_err(|_| offline("WebDAV response stream timed out"))?;
            match next_chunk {
                Some(chunk) => {
                    let chunk = chunk.map_err(|_| offline("WebDAV response interrupted"))?;
                    if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                        return Err(protocol("WebDAV response exceeds size limit"));
                    }
                    bytes.extend_from_slice(&chunk);
                }
                None => break,
            }
        }
        Ok(bytes)
    }

    async fn put_with_condition(
        &self,
        path: &RemotePath,
        bytes: &[u8],
        header: (&str, &str),
    ) -> CloudResult<()> {
        let response = self
            .request(Method::PUT, path)?
            .header(header.0, header.1)
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(map_reqwest)?;
        self.expect_success(response).await?;
        Ok(())
    }

    /// 发送幂等请求并在网关瞬断时重试一次；返回原始响应，状态码语义由调用方解释。
    async fn send_idempotent(
        &self,
        build: impl Fn() -> CloudResult<reqwest::RequestBuilder>,
    ) -> CloudResult<Response> {
        let mut attempt = 1usize;
        loop {
            let response = match build()?.send().await {
                Ok(response) => response,
                Err(error) => {
                    let error = map_reqwest(error);
                    if error.kind() == "protocol" && attempt < MAX_IDEMPOTENT_ATTEMPTS {
                        attempt += 1;
                        tokio::time::sleep(IDEMPOTENT_RETRY_DELAY).await;
                        continue;
                    }
                    return Err(error);
                }
            };
            if !is_transient_gateway_status(response.status()) || attempt >= MAX_IDEMPOTENT_ATTEMPTS
            {
                return Ok(response);
            }
            attempt += 1;
            tokio::time::sleep(IDEMPOTENT_RETRY_DELAY).await;
        }
    }
}

fn is_transient_gateway_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT
    )
}

#[async_trait]
impl CloudBackend for WebDavBackend {
    async fn list_depth_one(&self, path: &RemotePath) -> CloudResult<Vec<RemoteEntry>> {
        let method = Method::from_bytes(b"PROPFIND").expect("static PROPFIND method");
        let response = self
            .send_idempotent(|| {
                Ok(self
                    .request(method.clone(), path)?
                    .header("Depth", "1")
                    .header("Content-Type", "application/xml")
                    .body(PROPFIND_BODY))
            })
            .await?;
        let response = self.expect_success(response).await?;
        parse_multistatus(&Self::read_limited(response).await?)
    }

    async fn create_collection(&self, path: &RemotePath) -> CloudResult<()> {
        let method = Method::from_bytes(b"MKCOL").expect("static MKCOL method");
        let mut current = RemotePath::root();
        for segment in path.segments() {
            current = current.join(segment)?;
            let response = self
                .send_idempotent(|| self.request(method.clone(), &current))
                .await?;
            if !(response.status().is_success()
                || response.status() == StatusCode::METHOD_NOT_ALLOWED)
            {
                return Err(map_status(response.status()));
            }
        }
        Ok(())
    }

    async fn get(&self, path: &RemotePath) -> CloudResult<RemoteObject> {
        let response = self
            .send_idempotent(|| self.request(Method::GET, path))
            .await?;
        let response = self.expect_success(response).await?;
        let etag = response
            .headers()
            .get("etag")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        Ok(RemoteObject {
            bytes: Self::read_limited(response).await?,
            etag,
        })
    }

    async fn put_immutable(&self, path: &RemotePath, bytes: &[u8]) -> CloudResult<()> {
        self.put_if_absent(path, bytes).await
    }

    async fn put_if_match(&self, path: &RemotePath, bytes: &[u8], etag: &str) -> CloudResult<()> {
        self.put_with_condition(path, bytes, ("If-Match", etag))
            .await
    }

    async fn put_if_absent(&self, path: &RemotePath, bytes: &[u8]) -> CloudResult<()> {
        self.put_with_condition(path, bytes, ("If-None-Match", "*"))
            .await
    }

    async fn delete(&self, path: &RemotePath) -> CloudResult<()> {
        let response = self
            .request(Method::DELETE, path)?
            .send()
            .await
            .map_err(map_reqwest)?;
        if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(map_status(response.status()))
        }
    }

    async fn test_capabilities(&self) -> CloudResult<()> {
        let probe = RemotePath::parse(&format!(".acm-probe-{}", Uuid::new_v4()))?;
        let object = probe.join("probe.bin")?;
        let result = async {
            self.create_collection(&probe).await?;
            self.put_if_absent(&object, b"one").await?;
            let first = self.get(&object).await?;
            if first.bytes != b"one" {
                return Err(protocol("WebDAV probe content mismatch"));
            }
            let etag = first
                .etag
                .ok_or_else(|| protocol("WebDAV server did not return ETag"))?;
            if !self
                .put_if_match(&object, b"bad", "\"invalid\"")
                .await
                .is_err_and(|error| error.is_precondition())
            {
                return Err(protocol("WebDAV server ignored invalid If-Match"));
            }
            self.put_if_match(&object, b"two", &etag).await?;
            let listed = self.list_depth_one(&probe).await?;
            if !listed.iter().any(|entry| entry.name == "probe.bin") {
                return Err(protocol("WebDAV depth-one listing omitted probe"));
            }
            Ok(())
        }
        .await;
        let _ = self.delete(&object).await;
        let _ = self.delete(&probe).await;
        result
    }
}

fn parse_multistatus(bytes: &[u8]) -> CloudResult<Vec<RemoteEntry>> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    let mut entries = Vec::new();
    let mut href = None;
    let mut etag = None;
    let mut size = None;
    let mut collection = false;
    let mut field: Option<&'static str> = None;
    let mut current_text = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => match event.local_name().as_ref() {
                b"response" => {
                    href = None;
                    etag = None;
                    size = None;
                    collection = false;
                    field = None;
                    current_text.clear();
                }
                b"href" => {
                    field = Some("href");
                    current_text.clear();
                }
                b"getetag" => {
                    field = Some("etag");
                    current_text.clear();
                }
                b"getcontentlength" => {
                    field = Some("size");
                    current_text.clear();
                }
                b"collection" => collection = true,
                _ => {}
            },
            Ok(Event::Empty(event)) if event.local_name().as_ref() == b"collection" => {
                collection = true;
            }
            Ok(Event::Text(text)) => {
                if field.is_some() {
                    let raw = String::from_utf8_lossy(text.as_ref());
                    if let Ok(unescaped) = quick_xml::escape::unescape(&raw) {
                        current_text.push_str(&unescaped);
                    } else {
                        current_text.push_str(&raw);
                    }
                }
            }
            Ok(Event::GeneralRef(entity)) => {
                if field.is_some() {
                    let name: &[u8] = entity.as_ref();
                    match name {
                        b"amp" => current_text.push('&'),
                        b"lt" => current_text.push('<'),
                        b"gt" => current_text.push('>'),
                        b"quot" => current_text.push('"'),
                        b"apos" => current_text.push('\''),
                        _ => {
                            let raw = String::from_utf8_lossy(name);
                            if let Ok(unescaped) =
                                quick_xml::escape::unescape(&format!("&{};", raw))
                            {
                                current_text.push_str(&unescaped);
                            }
                        }
                    }
                }
            }
            Ok(Event::CData(cdata)) => {
                if field.is_some() {
                    current_text.push_str(&String::from_utf8_lossy(cdata.as_ref()));
                }
            }
            Ok(Event::End(event)) => match event.local_name().as_ref() {
                b"href" if field == Some("href") => {
                    href = Some(current_text.clone());
                    field = None;
                }
                b"getetag" if field == Some("etag") => {
                    etag = Some(current_text.clone());
                    field = None;
                }
                b"getcontentlength" if field == Some("size") => {
                    size = current_text.trim().parse().ok();
                    field = None;
                }
                b"response" => {
                    if let Some(href) = href.take() {
                        let name = href
                            .trim_end_matches('/')
                            .rsplit('/')
                            .next()
                            .unwrap_or_default()
                            .to_owned();
                        if !name.is_empty() {
                            entries.push(RemoteEntry {
                                name,
                                is_collection: collection,
                                etag: etag.take(),
                                size,
                            });
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => return Err(protocol("invalid WebDAV multistatus XML")),
        }
    }
    Ok(entries)
}

fn map_reqwest(error: reqwest::Error) -> CloudError {
    if error.is_timeout() || error.is_connect() {
        offline("WebDAV endpoint is offline")
    } else {
        protocol("WebDAV request failed")
    }
}

fn map_status(status: StatusCode) -> CloudError {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            CloudError::new(CloudErrorKind::Auth, "WebDAV authentication failed")
        }
        StatusCode::PRECONDITION_FAILED | StatusCode::CONFLICT => {
            CloudError::new(CloudErrorKind::Precondition, "WebDAV precondition failed")
        }
        StatusCode::NOT_FOUND => {
            CloudError::new(CloudErrorKind::NotFound, "WebDAV object not found")
        }
        _ => protocol("WebDAV protocol error"),
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
    use super::*;
    use crate::sync::{backend::CloudBackend, test_server::TestWebDav};
    use std::{
        io::{Read as _, Write as _},
        net::TcpListener,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    /// 每接受一个连接应答一次固定报文；记录连接数供重试断言使用。
    fn scripted_server(
        responses: &'static [&'static str],
        connections: Arc<AtomicUsize>,
    ) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for (index, stream) in listener.incoming().enumerate() {
                connections.fetch_add(1, Ordering::SeqCst);
                let mut stream = stream.unwrap();
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                let scripted = responses.get(index).or_else(|| responses.last()).unwrap();
                let _ = stream.write_all(scripted.as_bytes());
            }
        });
        address
    }

    #[test]
    fn treats_only_gateway_statuses_as_transient_for_idempotent_reads() {
        assert!(is_transient_gateway_status(StatusCode::BAD_GATEWAY));
        assert!(is_transient_gateway_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(is_transient_gateway_status(StatusCode::GATEWAY_TIMEOUT));
        assert!(!is_transient_gateway_status(StatusCode::NOT_FOUND));
        assert!(!is_transient_gateway_status(StatusCode::UNAUTHORIZED));
    }

    #[test]
    fn retries_transient_gateway_responses_for_idempotent_reads() {
        let connections = Arc::new(AtomicUsize::new(0));
        let address = scripted_server(
            &[
                "HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
            ],
            Arc::clone(&connections),
        );
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(async {
            let backend =
                WebDavBackend::new(&format!("http://{address}/"), "user", "pass").unwrap();
            backend.get(&RemotePath::parse("probe.bin").unwrap()).await
        });
        assert_eq!(result.unwrap().bytes, b"ok");
        assert_eq!(connections.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn does_not_retry_conditional_writes_on_transient_gateway_responses() {
        let connections = Arc::new(AtomicUsize::new(0));
        let address = scripted_server(
            &["HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"],
            Arc::clone(&connections),
        );
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let error = runtime
            .block_on(async {
                let backend =
                    WebDavBackend::new(&format!("http://{address}/"), "user", "pass").unwrap();
                backend
                    .put_if_absent(&RemotePath::parse("probe.bin").unwrap(), b"one")
                    .await
            })
            .unwrap_err();
        assert_eq!(error.kind(), "protocol");
        // 留出重试窗口：若有人给条件写加上了重试，这里的连接数会变成 2。
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(connections.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn webdav_enforces_auth_listing_and_conditional_updates() {
        let server = TestWebDav::start("user", "pass").await;
        let client = server.client("user", "pass").unwrap();
        let v1 = crate::sync::backend::RemotePath::parse("v1").unwrap();
        let head = crate::sync::backend::RemotePath::parse("v1/head.json").unwrap();
        client.create_collection(&v1).await.unwrap();
        client.put_if_absent(&head, b"one").await.unwrap();
        let object = client.get(&head).await.unwrap();
        assert!(client.put_if_match(&head, b"two", "wrong").await.is_err());
        client
            .put_if_match(&head, b"two", object.etag.as_deref().unwrap())
            .await
            .unwrap();
        let listed = client.list_depth_one(&v1).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "head.json");

        let unauthenticated = server.client("user", "wrong").unwrap();
        let error = unauthenticated.get(&head).await.unwrap_err();
        assert_eq!(error.kind(), "auth");
        assert!(!error.to_string().contains("wrong"));
    }

    #[tokio::test]
    async fn capability_probe_exercises_full_contract_and_cleans_up() {
        let server = TestWebDav::start("user", "pass").await;
        let client = server.client("user", "pass").unwrap();
        client.test_capabilities().await.unwrap();
        assert!(
            client
                .list_depth_one(&crate::sync::backend::RemotePath::root())
                .await
                .unwrap()
                .is_empty()
        );
        let methods = server.methods().await;
        for method in ["PROPFIND", "MKCOL", "GET", "PUT", "DELETE"] {
            assert!(methods.iter().any(|value| value == method));
        }
    }

    #[test]
    fn parse_multistatus_unescapes_xml_entities_and_buffers_text() {
        let xml = br#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/remote.php/dav/files/user/test%20dir/file&amp;name&lt;1&gt;.json</D:href>
    <D:propstat>
      <D:prop>
        <D:getetag>&quot;etag-&amp;-123&quot;</D:getetag>
        <D:getcontentlength> 1048576 </D:getcontentlength>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
  <D:response>
    <D:href>/remote.php/dav/files/user/folder/</D:href>
    <D:propstat>
      <D:prop>
        <D:resourcetype><D:collection/></D:resourcetype>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;
        let entries = super::parse_multistatus(xml).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "file&name<1>.json");
        assert_eq!(entries[0].etag.as_deref(), Some("\"etag-&-123\""));
        assert_eq!(entries[0].size, Some(1048576));
        assert!(!entries[0].is_collection);
        assert_eq!(entries[1].name, "folder");
        assert!(entries[1].is_collection);
    }

    #[tokio::test]
    async fn webdav_times_out_when_server_hangs() {
        let client = WebDavBackend::new_with_timeouts(
            "http://192.0.2.1:81/",
            "user",
            "pass",
            Duration::from_millis(50),
            Duration::from_millis(100),
        )
        .unwrap();

        let head = crate::sync::backend::RemotePath::parse("head.json").unwrap();
        let err = client.get(&head).await.unwrap_err();
        assert_eq!(err.kind(), "offline");
    }
}
