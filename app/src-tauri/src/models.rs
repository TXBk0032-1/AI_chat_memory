use serde::{Deserialize, Serialize};
use serde_json::Value;

/// User-selected language. `System` is resolved by the frontend using the
/// browser/system language before synchronising native UI copy.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum LanguagePreference {
    #[default]
    #[serde(rename = "system")]
    System,
    #[serde(rename = "zh-CN")]
    ZhCn,
    #[serde(rename = "en-US")]
    EnUs,
}

/// Locale values accepted by the native window/tray synchronisation command.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SupportedLocale {
    #[serde(rename = "zh-CN")]
    ZhCn,
    #[serde(rename = "en-US")]
    EnUs,
}

impl LanguagePreference {
    pub const fn supported_locale(self) -> Option<SupportedLocale> {
        match self {
            Self::System => None,
            Self::ZhCn => Some(SupportedLocale::ZhCn),
            Self::EnUs => Some(SupportedLocale::EnUs),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub metadata: Value,
    pub created_at: Option<String>,
    pub seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub platform: String,
    pub platform_session_id: String,
    pub title: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub imported_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionOpen {
    #[serde(flatten)]
    pub summary: SessionSummary,
    pub message_count: usize,
    pub has_branches: bool,
    pub start_seq: i64,
    pub messages: Vec<Message>,
    pub references: Vec<Reference>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Reference {
    pub cite_index: i64,
    pub url: String,
    pub title: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchHitField {
    Content,
    Thinking,
    Semantic,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SessionSearchHit {
    pub message_id: String,
    pub seq: i64,
    pub field: SearchHitField,
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BranchNode {
    pub message_id: String,
    pub seq: i64,
    pub role: String,
    pub node_id: String,
    pub parent_node_id: String,
    pub children_node_ids: Vec<String>,
    pub preview: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BranchOverview {
    pub nodes: Vec<BranchNode>,
    pub default_leaf_node_id: String,
}

#[derive(Debug, Clone)]
pub struct NormalizedSession {
    pub id: String,
    pub platform: String,
    pub platform_session_id: String,
    pub title: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub imported_at: String,
    pub messages: Vec<NormalizedMessage>,
    pub raw_data: Value,
}

#[derive(Debug, Clone)]
pub struct NormalizedMessage {
    pub role: String,
    pub content: String,
    pub metadata: Value,
    pub created_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ImportRequest {
    pub platform: String,
    pub sessions: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportResponse {
    pub imported: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Keyword,
    Semantic,
    #[default]
    Hybrid,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub platform: Option<String>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    #[serde(default)]
    pub mode: Option<SearchMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticStatus {
    Disabled,
    Ready,
    Indexing,
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionList {
    pub sessions: Vec<SessionSummary>,
    pub total: usize,
    pub search_mode: SearchMode,
    pub semantic_status: SemanticStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", content = "message", rename_all = "snake_case")]
pub enum ApiStatus {
    Starting,
    Running,
    Failed(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct DesktopApiStatus {
    pub service: ApiStatus,
    pub userscript_connected: bool,
    pub last_userscript_request_at: Option<u64>,
    pub mcp: crate::local_services::LocalServiceStatus,
    pub mcp_url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingBackendKind {
    #[default]
    Local,
    Ollama,
    LlamaCpp,
    OpenaiCompatible,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LocalEmbeddingDevice {
    #[default]
    Auto,
    Cuda,
    Cpu,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LocalEmbeddingDType {
    #[default]
    Auto,
    F16,
    F32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LocalEmbeddingSettings {
    #[serde(default = "default_local_model")]
    pub model: String,
    #[serde(default)]
    pub model_path: Option<String>,
    #[serde(default)]
    pub device: LocalEmbeddingDevice,
    #[serde(default)]
    pub dtype: LocalEmbeddingDType,
}

impl Default for LocalEmbeddingSettings {
    fn default() -> Self {
        Self {
            model: default_local_model(),
            model_path: None,
            device: LocalEmbeddingDevice::Auto,
            dtype: LocalEmbeddingDType::Auto,
        }
    }
}

fn default_local_model() -> String {
    "BAAI/bge-small-zh-v1.5".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RemoteEmbeddingSettings {
    #[serde(default = "default_ollama_url")]
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_remote_model")]
    pub model: String,
    #[serde(default)]
    pub dimensions: Option<usize>,
}

impl Default for RemoteEmbeddingSettings {
    fn default() -> Self {
        Self {
            base_url: default_ollama_url(),
            api_key: None,
            model: default_remote_model(),
            dimensions: None,
        }
    }
}

fn default_ollama_url() -> String {
    "http://127.0.0.1:11434".into()
}

fn default_openai_url() -> String {
    "http://127.0.0.1:8080/v1".into()
}

fn default_remote_model() -> String {
    "nomic-embed-text".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticSearchSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub default_mode: SearchMode,
    #[serde(default)]
    pub backend: EmbeddingBackendKind,
    #[serde(default)]
    pub local: LocalEmbeddingSettings,
    #[serde(default = "default_ollama_settings")]
    pub ollama: RemoteEmbeddingSettings,
    #[serde(default = "default_llama_cpp_settings")]
    pub llama_cpp: RemoteEmbeddingSettings,
    #[serde(default = "default_openai_settings")]
    pub openai_compatible: RemoteEmbeddingSettings,
}

impl Default for SemanticSearchSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            default_mode: SearchMode::Hybrid,
            backend: EmbeddingBackendKind::Local,
            local: LocalEmbeddingSettings::default(),
            ollama: default_ollama_settings(),
            llama_cpp: default_llama_cpp_settings(),
            openai_compatible: default_openai_settings(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_ollama_settings() -> RemoteEmbeddingSettings {
    RemoteEmbeddingSettings {
        base_url: default_ollama_url(),
        api_key: None,
        model: default_remote_model(),
        dimensions: None,
    }
}

fn default_llama_cpp_settings() -> RemoteEmbeddingSettings {
    RemoteEmbeddingSettings {
        base_url: default_openai_url(),
        api_key: None,
        model: "bge-small-zh-v1.5".into(),
        dimensions: None,
    }
}

fn default_openai_settings() -> RemoteEmbeddingSettings {
    RemoteEmbeddingSettings {
        base_url: "https://api.openai.com/v1".into(),
        api_key: None,
        model: "text-embedding-3-small".into(),
        dimensions: None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub setup_complete: bool,
    pub secret_enabled: bool,
    pub secret: Option<String>,
    pub allowed_origins: Vec<String>,
    #[serde(default)]
    pub data_directory: Option<String>,
    #[serde(default)]
    pub close_behavior: CloseBehavior,
    #[serde(default)]
    pub tray_click_behavior: TrayClickBehavior,
    #[serde(default)]
    pub theme: ThemePreference,
    #[serde(default)]
    pub language: LanguagePreference,
    #[serde(default)]
    pub semantic_search: SemanticSearchSettings,
    #[serde(default = "default_true")]
    pub mcp_enabled: bool,
    #[serde(default)]
    pub cloud_sync: CloudSyncSettings,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CloudBackendKind {
    #[default]
    Webdav,
    S3,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct S3CloudSyncSettings {
    #[serde(default)]
    pub endpoint_url: String,
    #[serde(default = "default_s3_region")]
    pub region: String,
    #[serde(default)]
    pub bucket: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub force_path_style: bool,
}

impl Default for S3CloudSyncSettings {
    fn default() -> Self {
        Self {
            endpoint_url: String::new(),
            region: default_s3_region(),
            bucket: String::new(),
            prefix: String::new(),
            force_path_style: false,
        }
    }
}

fn default_s3_region() -> String {
    "us-east-1".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloudSyncSettings {
    #[serde(default)]
    pub backend: CloudBackendKind,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub connection_verified: bool,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub root_path: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub encryption_enabled: bool,
    #[serde(default)]
    pub s3: S3CloudSyncSettings,
    #[serde(default = "default_remote_id")]
    pub remote_id: String,
    #[serde(default = "default_vault_id")]
    pub vault_id: String,
    #[serde(default = "default_generation_id")]
    pub generation_id: String,
}

impl Default for CloudSyncSettings {
    fn default() -> Self {
        Self {
            backend: CloudBackendKind::Webdav,
            enabled: false,
            connection_verified: false,
            base_url: String::new(),
            root_path: String::new(),
            username: String::new(),
            encryption_enabled: false,
            s3: S3CloudSyncSettings::default(),
            remote_id: default_remote_id(),
            vault_id: default_vault_id(),
            generation_id: default_generation_id(),
        }
    }
}

impl CloudSyncSettings {
    pub fn normalize(&mut self) {
        self.s3.endpoint_url = self.s3.endpoint_url.trim().trim_end_matches('/').to_owned();
        self.s3.region = self.s3.region.trim().to_owned();
        if self.s3.region.is_empty() {
            self.s3.region = default_s3_region();
        }
        self.s3.bucket = self.s3.bucket.trim().to_owned();
        self.s3.prefix = self.s3.prefix.trim().trim_matches('/').to_owned();
        self.remote_id = self.remote_id.trim().to_owned();
        self.vault_id = self.vault_id.trim().to_owned();
        self.generation_id = self.generation_id.trim().to_owned();
        if self.remote_id.is_empty() {
            self.remote_id = default_remote_id();
        }
        if self.vault_id.is_empty() {
            self.vault_id = default_vault_id();
        }
        if self.generation_id.is_empty() {
            self.generation_id = default_generation_id();
        }
    }

    pub fn rotate_remote_identity(&mut self) {
        let id = uuid::Uuid::new_v4().simple().to_string();
        self.remote_id = format!("remote-{id}");
        self.vault_id = format!("vault-{id}");
        self.generation_id = format!("generation-{id}");
    }
}

fn default_remote_id() -> String {
    "default".into()
}

fn default_vault_id() -> String {
    "default".into()
}

fn default_generation_id() -> String {
    "generation-1".into()
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum CloudSyncState {
    #[default]
    Disabled,
    Idle,
    Syncing,
    Offline,
    NeedsUnlock,
    AuthError,
    ProtocolError,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RemoteDeviceStatus {
    pub device_id: String,
    pub display_name: String,
    pub last_seen_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CloudSyncStatus {
    pub state: CloudSyncState,
    pub last_success_at: Option<String>,
    pub pending_mutations: i64,
    pub last_error_code: Option<String>,
    pub last_error_message: Option<String>,
    pub devices: Vec<RemoteDeviceStatus>,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum CloudCredentialInput {
    Webdav {
        password: String,
        sync_password: Option<String>,
    },
    S3 {
        access_key_id: String,
        secret_access_key: String,
        session_token: Option<String>,
        sync_password: Option<String>,
    },
}

impl std::fmt::Debug for CloudCredentialInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Webdav { .. } => formatter
                .debug_struct("Webdav")
                .field("credentials", &"[REDACTED]")
                .finish(),
            Self::S3 { .. } => formatter
                .debug_struct("S3")
                .field("credentials", &"[REDACTED]")
                .finish(),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CloudConnectionTestResult {
    pub ok: bool,
    pub message: String,
    pub supports_conditional_write: bool,
    pub cloud_sync: CloudSyncSettings,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseBehavior {
    #[default]
    Ask,
    HideToTray,
    Exit,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrayClickBehavior {
    #[default]
    ShowMenu,
    OpenWindow,
    NoAction,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            setup_complete: false,
            secret_enabled: false,
            secret: None,
            allowed_origins: vec![
                "https://chat.deepseek.com".into(),
                "https://www.doubao.com".into(),
                "https://kimi.com".into(),
                "https://www.kimi.com".into(),
            ],
            data_directory: None,
            close_behavior: CloseBehavior::Ask,
            tray_click_behavior: TrayClickBehavior::ShowMenu,
            theme: ThemePreference::System,
            language: LanguagePreference::System,
            semantic_search: SemanticSearchSettings::default(),
            mcp_enabled: true,
            cloud_sync: CloudSyncSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ReindexProgress {
    pub stage: String,
    pub total_sessions: usize,
    pub processed_sessions: usize,
    pub total_chunks: i64,
    pub ready_chunks: i64,
    pub pending_chunks: i64,
    pub fraction: f32,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticRuntimeStatus {
    pub enabled: bool,
    pub status: SemanticStatus,
    pub backend: EmbeddingBackendKind,
    pub model_id: String,
    pub dimensions: Option<usize>,
    pub pending_chunks: i64,
    pub ready_chunks: i64,
    pub message: Option<String>,
    pub local_model_ready: bool,
    pub local_model_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dtype: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reindex: Option<ReindexProgress>,
}

#[cfg(test)]
mod cloud_sync_tests {
    use super::{
        AppSettings, CloudBackendKind, CloudCredentialInput, CloudSyncSettings, S3CloudSyncSettings,
    };
    use serde_json::json;

    #[test]
    fn legacy_cloud_sync_settings_default_to_webdav() {
        let settings: AppSettings = serde_json::from_value(json!({
            "setup_complete": true,
            "secret_enabled": false,
            "secret": null,
            "allowed_origins": [],
            "cloud_sync": {
                "enabled": true,
                "base_url": "https://dav.example.test/archive",
                "root_path": "chat-memory",
                "username": "alice",
                "encryption_enabled": false
            }
        }))
        .unwrap();

        assert_eq!(settings.cloud_sync.backend, CloudBackendKind::Webdav);
        assert_eq!(settings.cloud_sync.s3.region, "us-east-1");
        assert!(settings.cloud_sync.s3.endpoint_url.is_empty());
    }

    #[test]
    fn s3_prefix_is_normalized_without_leading_or_trailing_slashes() {
        let mut settings = CloudSyncSettings {
            backend: CloudBackendKind::S3,
            s3: S3CloudSyncSettings {
                prefix: "//team/archive///".into(),
                ..S3CloudSyncSettings::default()
            },
            ..CloudSyncSettings::default()
        };

        settings.normalize();

        assert_eq!(settings.s3.prefix, "team/archive");
    }

    #[test]
    fn s3_settings_serialization_never_contains_credentials() {
        let settings = CloudSyncSettings {
            backend: CloudBackendKind::S3,
            s3: S3CloudSyncSettings {
                endpoint_url: "https://s3.example.test".into(),
                region: "auto".into(),
                bucket: "archive".into(),
                prefix: "team/chat".into(),
                force_path_style: true,
            },
            ..CloudSyncSettings::default()
        };
        let wire = serde_json::to_string(&settings).unwrap();

        assert!(wire.contains("s3.example.test"));
        for secret in [
            "access_key_id",
            "secret_access_key",
            "session_token",
            "sync_password",
        ] {
            assert!(!wire.contains(secret));
        }
    }

    #[test]
    fn cloud_credentials_are_a_backend_tagged_union() {
        let webdav: CloudCredentialInput = serde_json::from_value(json!({
            "backend": "webdav",
            "password": "dav-secret",
            "sync_password": "sync-secret"
        }))
        .unwrap();
        let s3: CloudCredentialInput = serde_json::from_value(json!({
            "backend": "s3",
            "access_key_id": "AKID",
            "secret_access_key": "secret",
            "session_token": "token",
            "sync_password": null
        }))
        .unwrap();

        assert!(matches!(webdav, CloudCredentialInput::Webdav { .. }));
        assert!(matches!(s3, CloudCredentialInput::S3 { .. }));
    }

    #[test]
    fn cloud_credential_debug_output_is_redacted() {
        let credentials: CloudCredentialInput = serde_json::from_value(json!({
            "backend": "s3",
            "access_key_id": "AKID-SENSITIVE",
            "secret_access_key": "SECRET-SENSITIVE",
            "session_token": "TOKEN-SENSITIVE",
            "sync_password": "SYNC-SENSITIVE"
        }))
        .unwrap();

        let debug = format!("{credentials:?}");

        assert!(debug.contains("REDACTED"));
        for value in [
            "AKID-SENSITIVE",
            "SECRET-SENSITIVE",
            "TOKEN-SENSITIVE",
            "SYNC-SENSITIVE",
        ] {
            assert!(!debug.contains(value));
        }
    }

    #[test]
    fn rotating_remote_identity_replaces_vault_and_generation() {
        let mut settings = CloudSyncSettings::default();
        let previous = (
            settings.remote_id.clone(),
            settings.vault_id.clone(),
            settings.generation_id.clone(),
        );

        settings.rotate_remote_identity();

        assert_ne!(settings.remote_id, previous.0);
        assert_ne!(settings.vault_id, previous.1);
        assert_ne!(settings.generation_id, previous.2);
        assert!(settings.remote_id.starts_with("remote-"));
        assert!(settings.vault_id.starts_with("vault-"));
        assert!(settings.generation_id.starts_with("generation-"));
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelDownloadProgress {
    pub stage: String,
    pub file: Option<String>,
    pub file_index: usize,
    pub file_count: usize,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub fraction: f32,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingHealth {
    pub ok: bool,
    pub backend: EmbeddingBackendKind,
    pub model_id: String,
    pub dimensions: Option<usize>,
    pub message: String,
}
