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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDetail {
    #[serde(flatten)]
    pub summary: SessionSummary,
    pub messages: Vec<Message>,
    pub raw_data: Option<Value>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub platform: Option<String>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionList {
    pub sessions: Vec<SessionSummary>,
    pub total: usize,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub setup_complete: bool,
    pub secret_enabled: bool,
    pub secret: Option<String>,
    pub allowed_origins: Vec<String>,
    pub migrated_legacy_database: bool,
    #[serde(default)]
    pub data_directory: Option<String>,
    #[serde(default)]
    pub close_behavior: CloseBehavior,
    #[serde(default)]
    pub tray_click_behavior: TrayClickBehavior,
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
            migrated_legacy_database: false,
            data_directory: None,
            close_behavior: CloseBehavior::Ask,
            tray_click_behavior: TrayClickBehavior::ShowMenu,
        }
    }
}
