use crate::sync::backend::{
    CloudBackend, CloudError, CloudErrorKind, CloudResult, RemoteEntry, RemoteObject, RemotePath,
};
use async_trait::async_trait;
use futures::StreamExt;
use quick_xml::{Reader, events::Event};
use reqwest::{Client, Method, Response, StatusCode, redirect::Policy};
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

const MAX_RESPONSE_BYTES: usize = 128 * 1024 * 1024;

pub struct WebDavBackend {
    base_url: Url,
    username: String,
    password: Zeroizing<String>,
    client: Client,
}

impl WebDavBackend {
    pub fn new(base_url: &str, username: &str, password: &str) -> CloudResult<Self> {
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
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| offline("WebDAV response interrupted"))?;
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(protocol("WebDAV response exceeds size limit"));
            }
            bytes.extend_from_slice(&chunk);
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
}

#[async_trait]
impl CloudBackend for WebDavBackend {
    async fn list_depth_one(&self, path: &RemotePath) -> CloudResult<Vec<RemoteEntry>> {
        let method = Method::from_bytes(b"PROPFIND").expect("static PROPFIND method");
        let response = self
            .request(method, path)?
            .header("Depth", "1")
            .header("Content-Type", "application/xml")
            .body("<?xml version=\"1.0\"?><propfind xmlns=\"DAV:\"><prop><getetag/><getcontentlength/><resourcetype/></prop></propfind>")
            .send()
            .await
            .map_err(map_reqwest)?;
        let response = self.expect_success(response).await?;
        parse_multistatus(&Self::read_limited(response).await?)
    }

    async fn create_collection(&self, path: &RemotePath) -> CloudResult<()> {
        let method = Method::from_bytes(b"MKCOL").expect("static MKCOL method");
        let mut current = RemotePath::root();
        for segment in path.segments() {
            current = current.join(segment)?;
            let response = self
                .request(method.clone(), &current)?
                .send()
                .await
                .map_err(map_reqwest)?;
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
            .request(Method::GET, path)?
            .send()
            .await
            .map_err(map_reqwest)?;
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
    use crate::sync::{backend::CloudBackend, test_server::TestWebDav};

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
}
