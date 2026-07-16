use serde::{Deserialize, Serialize};
use serde_json::Value;

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

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LocalEmbeddingSettings {
    #[serde(default = "default_local_model")]
    pub model: String,
    #[serde(default)]
    pub model_path: Option<String>,
}

impl Default for LocalEmbeddingSettings {
    fn default() -> Self {
        Self {
            model: default_local_model(),
            model_path: None,
        }
    }
}

fn default_local_model() -> String {
    "microsoft/harrier-oss-v1-270m".into()
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
        model: "harrier-oss-v1-270m".into(),
        dimensions: Some(640),
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
    pub semantic_search: SemanticSearchSettings,
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
            semantic_search: SemanticSearchSettings::default(),
        }
    }
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
}

#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingHealth {
    pub ok: bool,
    pub backend: EmbeddingBackendKind,
    pub model_id: String,
    pub dimensions: Option<usize>,
    pub message: String,
}
