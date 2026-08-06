#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("cryptography error: {0}")]
    Crypto(String),
    #[error("credential storage error: {0}")]
    Credential(String),
    #[error("cloud sync error: {0}")]
    Cloud(#[from] crate::sync::backend::CloudError),
    #[error("cloud sync protocol error: {0}")]
    SyncProtocol(String),
    #[error("invalid data: {0}")]
    InvalidData(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("cancelled: {0}")]
    Cancelled(String),
}

pub type Result<T> = std::result::Result<T, AppError>;
