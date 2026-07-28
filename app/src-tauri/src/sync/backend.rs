use async_trait::async_trait;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudErrorKind {
    Auth,
    Offline,
    Precondition,
    NotFound,
    Protocol,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudError {
    kind: CloudErrorKind,
    message: &'static str,
}

impl CloudError {
    pub fn new(kind: CloudErrorKind, message: &'static str) -> Self {
        Self { kind, message }
    }

    pub fn kind(&self) -> &'static str {
        match self.kind {
            CloudErrorKind::Auth => "auth",
            CloudErrorKind::Offline => "offline",
            CloudErrorKind::Precondition => "precondition",
            CloudErrorKind::NotFound => "not_found",
            CloudErrorKind::Protocol => "protocol",
        }
    }

    pub fn is_precondition(&self) -> bool {
        self.kind == CloudErrorKind::Precondition
    }
}

impl fmt::Display for CloudError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for CloudError {}

pub type CloudResult<T> = std::result::Result<T, CloudError>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RemotePath(Vec<String>);

impl RemotePath {
    pub fn root() -> Self {
        Self(Vec::new())
    }

    pub fn parse(value: &str) -> CloudResult<Self> {
        if value.is_empty() {
            return Ok(Self::root());
        }
        if value.starts_with('/') || value.ends_with('/') || value.contains('\\') {
            return Err(CloudError::new(
                CloudErrorKind::Protocol,
                "invalid remote path",
            ));
        }
        let segments = value.split('/').map(str::to_owned).collect::<Vec<_>>();
        if segments.iter().any(|segment| {
            segment.is_empty()
                || segment == "."
                || segment == ".."
                || segment.contains(':')
                || segment.chars().any(char::is_control)
        }) {
            return Err(CloudError::new(
                CloudErrorKind::Protocol,
                "invalid remote path segment",
            ));
        }
        Ok(Self(segments))
    }

    pub fn join(&self, segment: &str) -> CloudResult<Self> {
        let child = Self::parse(segment)?;
        if child.0.len() != 1 {
            return Err(CloudError::new(
                CloudErrorKind::Protocol,
                "remote child must contain one segment",
            ));
        }
        let mut segments = self.0.clone();
        segments.extend(child.0);
        Ok(Self(segments))
    }

    pub fn segments(&self) -> &[String] {
        &self.0
    }

    pub fn display(&self) -> String {
        self.0.join("/")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteObject {
    pub bytes: Vec<u8>,
    pub etag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    pub name: String,
    pub is_collection: bool,
    pub etag: Option<String>,
    pub size: Option<u64>,
}

#[async_trait]
pub trait CloudBackend: Send + Sync {
    async fn list_depth_one(&self, path: &RemotePath) -> CloudResult<Vec<RemoteEntry>>;
    async fn create_collection(&self, path: &RemotePath) -> CloudResult<()>;
    async fn get(&self, path: &RemotePath) -> CloudResult<RemoteObject>;
    async fn put_immutable(&self, path: &RemotePath, bytes: &[u8]) -> CloudResult<()>;
    async fn put_if_match(&self, path: &RemotePath, bytes: &[u8], etag: &str) -> CloudResult<()>;
    async fn put_if_absent(&self, path: &RemotePath, bytes: &[u8]) -> CloudResult<()>;
    async fn delete(&self, path: &RemotePath) -> CloudResult<()>;
    async fn test_capabilities(&self) -> CloudResult<()>;
}
