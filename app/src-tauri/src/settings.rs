use rand::RngCore;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crate::{
    error::{AppError, Result},
    models::AppSettings,
    sync::credentials::{CredentialStore, SecretKind, SecretValue, SystemCredentialStore},
};

/// CFG-4: the userscript shared secret lives in the system credential store
/// under this fixed vault key, never in settings.json.
const USERSCRIPT_SECRET_KEY: &str = "userscript";
const CREDENTIAL_SERVICE: &str = "ai-chat-memory";

pub struct SettingsStore {
    path: PathBuf,
    value: RwLock<AppSettings>,
    credentials: Arc<dyn CredentialStore>,
}

impl SettingsStore {
    pub async fn load(path: PathBuf) -> Result<Self> {
        Self::load_with_credential_store(
            path,
            Arc::new(SystemCredentialStore::new(CREDENTIAL_SERVICE)),
        )
        .await
    }

    pub async fn load_with_credential_store(
        path: PathBuf,
        credentials: Arc<dyn CredentialStore>,
    ) -> Result<Self> {
        let mut value = if path.exists() {
            match serde_json::from_slice(&tokio::fs::read(&path).await?) {
                Ok(value) => value,
                Err(error) => {
                    let corrupt = path
                        .with_extension(format!("corrupt-{}.json", chrono::Utc::now().timestamp()));
                    tokio::fs::rename(&path, &corrupt).await?;
                    tracing::error!(%error, path=%corrupt.display(), "settings were corrupt; restored defaults");
                    AppSettings::default()
                }
            }
        } else {
            AppSettings::default()
        };
        if value.semantic_search.local.model == "microsoft/harrier-oss-v1-270m" {
            value.semantic_search.local.model = "BAAI/bge-small-zh-v1.5".into();
            value.semantic_search.local.model_path = None;
            tracing::info!("migrated default local embedding model to bge-small-zh-v1.5");
        }
        value.cloud_sync.normalize();
        // CFG-4: a legacy settings.json may still carry the plaintext userscript
        // secret. Migrate it once into the credential store and rewrite
        // settings.json without it. If the credential store is unavailable the
        // plaintext is kept so no secret is lost; migration is retried on the
        // next load.
        let mut migrated_plaintext_secret = false;
        if let Some(secret) = value.secret.clone().filter(|secret| !secret.is_empty()) {
            match credentials
                .set(
                    USERSCRIPT_SECRET_KEY,
                    SecretKind::UserscriptSecret,
                    SecretValue::new(secret),
                )
                .await
            {
                Ok(()) => {
                    value.secret = None;
                    migrated_plaintext_secret = true;
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "userscript secret migration into the credential store failed; \
                         the plaintext value stays in settings.json"
                    );
                }
            }
        }
        // Keep the runtime copy (and the get_settings command) working from the
        // credential store so userscript/MCP authorization and secret display
        // behave exactly as before.
        if value.secret_enabled && value.secret.is_none() {
            match credentials
                .get(USERSCRIPT_SECRET_KEY, SecretKind::UserscriptSecret)
                .await
            {
                Ok(Some(secret)) => value.secret = Some(secret.expose_secret().to_owned()),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(%error, "failed to read the userscript secret from the credential store");
                }
            }
        }
        let store = Self {
            path,
            value: RwLock::new(value.clone()),
            credentials,
        };
        if migrated_plaintext_secret && let Err(error) = store.persist(&value).await {
            tracing::warn!(
                %error,
                "failed to remove the plaintext userscript secret from settings.json"
            );
        }
        Ok(store)
    }
    pub fn current(&self) -> AppSettings {
        match self.value.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => {
                tracing::warn!("settings read lock poisoned, recovering data");
                poisoned.into_inner().clone()
            }
        }
    }
    pub async fn get(&self) -> AppSettings {
        self.current()
    }
    pub async fn update(&self, mut value: AppSettings) -> Result<AppSettings> {
        validate_origins(&value.allowed_origins)?;
        value.cloud_sync.normalize();
        self.apply_secret_policy(&mut value).await?;
        self.persist(&value).await?;
        match self.value.write() {
            Ok(mut guard) => *guard = value.clone(),
            Err(poisoned) => {
                tracing::warn!("settings write lock poisoned, recovering state");
                *poisoned.into_inner() = value.clone();
            }
        }
        tracing::info!(secret_enabled=value.secret_enabled, origin_count=value.allowed_origins.len(), theme=?value.theme, "application settings updated");
        Ok(value)
    }
    pub async fn rotate_secret(&self) -> Result<AppSettings> {
        let mut value = self.current();
        value.secret_enabled = true;
        value.secret = Some(generate_secret());
        let settings = self.update(value).await?;
        tracing::info!("userscript secret rotated");
        Ok(settings)
    }

    /// CFG-4: the shared secret is persisted only in the credential store.
    /// settings.json keeps just the `secret_enabled` flag; the in-memory value
    /// stays populated so userscript/MCP authorization and the settings UI keep
    /// working.
    async fn apply_secret_policy(&self, value: &mut AppSettings) -> Result<()> {
        if value.secret_enabled {
            let provided = value.secret.take().filter(|secret| !secret.is_empty());
            let secret = match provided {
                Some(secret) => secret,
                None => {
                    match self
                        .credentials
                        .get(USERSCRIPT_SECRET_KEY, SecretKind::UserscriptSecret)
                        .await?
                    {
                        Some(existing) => existing.expose_secret().to_owned(),
                        None => generate_secret(),
                    }
                }
            };
            self.credentials
                .set(
                    USERSCRIPT_SECRET_KEY,
                    SecretKind::UserscriptSecret,
                    SecretValue::new(secret.clone()),
                )
                .await?;
            value.secret = Some(secret);
        } else {
            value.secret = None;
            if let Err(error) = self
                .credentials
                .delete(USERSCRIPT_SECRET_KEY, SecretKind::UserscriptSecret)
                .await
            {
                tracing::warn!(%error, "failed to remove the disabled userscript secret from the credential store");
            }
        }
        Ok(())
    }

    /// Atomically writes settings.json with the userscript secret stripped, so
    /// the plaintext never reaches disk even when callers echo it back.
    async fn persist(&self, value: &AppSettings) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut persisted = value.clone();
        persisted.secret = None;
        let temporary = self.path.with_extension("json.tmp");
        let backup = self.path.with_extension("json.bak");
        tokio::fs::write(&temporary, serde_json::to_vec_pretty(&persisted)?).await?;
        if self.path.exists() {
            let _ = tokio::fs::remove_file(&backup).await;
            tokio::fs::rename(&self.path, &backup).await?;
        }
        if let Err(error) = tokio::fs::rename(&temporary, &self.path).await {
            if backup.exists() {
                let _ = tokio::fs::rename(&backup, &self.path).await;
            }
            return Err(error.into());
        }
        let _ = tokio::fs::remove_file(&backup).await;
        Ok(())
    }
}

fn validate_origins(origins: &[String]) -> Result<()> {
    for origin in origins {
        if origin.contains('*')
            || !(origin.starts_with("https://") || origin.starts_with("http://"))
            || origin.ends_with('/')
        {
            return Err(AppError::Configuration(format!("invalid origin: {origin}")));
        }
    }
    Ok(())
}

fn generate_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        models::{LanguagePreference, SupportedLocale},
        sync::credentials::MemoryCredentialStore,
    };
    use std::path::Path;

    async fn test_store() -> (SettingsStore, PathBuf, Arc<MemoryCredentialStore>) {
        let root = std::env::temp_dir().join(format!(
            "acm-settings-cfg4-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let credentials = Arc::new(MemoryCredentialStore::default());
        let store = SettingsStore::load_with_credential_store(
            root.join("settings.json"),
            credentials.clone(),
        )
        .await
        .unwrap();
        (store, root, credentials)
    }

    async fn assert_settings_json_has_no_secret(root: &Path) {
        let raw = tokio::fs::read(root.join("settings.json")).await.unwrap();
        let text = String::from_utf8(raw).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(
            parsed.get("secret").is_none_or(|value| value.is_null()),
            "settings.json must not carry the plaintext secret: {text}"
        );
    }

    #[tokio::test]
    async fn recovers_from_corrupt_settings() {
        let root = std::env::temp_dir().join(format!("acm-test-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&root).await.unwrap();
        let path = root.join("settings.json");
        tokio::fs::write(&path, b"not-json").await.unwrap();
        let store = SettingsStore::load(path).await.unwrap();
        assert!(!store.get().await.setup_complete);
        assert!(root.read_dir().unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("settings.corrupt-")
        }));
        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[test]
    fn loads_settings_created_before_desktop_preferences() {
        let settings: AppSettings = serde_json::from_str(
            r#"{"setup_complete":true,"secret_enabled":false,"secret":null,"allowed_origins":[]}"#,
        )
        .unwrap();
        assert_eq!(settings.close_behavior, crate::models::CloseBehavior::Ask);
        assert_eq!(
            settings.tray_click_behavior,
            crate::models::TrayClickBehavior::ShowMenu
        );
        assert!(settings.data_directory.is_none());
        assert_eq!(settings.theme, crate::models::ThemePreference::System);
        assert!(settings.semantic_search.enabled);
        assert_eq!(
            settings.semantic_search.default_mode,
            crate::models::SearchMode::Hybrid
        );
    }

    #[test]
    fn disables_secret_validation_by_default() {
        let settings = AppSettings::default();
        assert!(!settings.secret_enabled);
        assert!(settings.secret.is_none());
        assert_eq!(settings.theme, crate::models::ThemePreference::System);
    }

    #[test]
    fn defaults_language_for_legacy_json_and_round_trips_supported_values() {
        let legacy: AppSettings = serde_json::from_str(
            r#"{"setup_complete":true,"secret_enabled":false,"secret":null,"allowed_origins":[]}"#,
        )
        .unwrap();
        assert_eq!(legacy.language, LanguagePreference::System);

        for (json, value) in [
            (r#""system""#, LanguagePreference::System),
            (r#""zh-CN""#, LanguagePreference::ZhCn),
            (r#""en-US""#, LanguagePreference::EnUs),
        ] {
            let decoded: LanguagePreference = serde_json::from_str(json).unwrap();
            assert_eq!(decoded, value);
            assert_eq!(serde_json::to_string(&decoded).unwrap(), json);
        }

        for (json, value) in [
            (r#""zh-CN""#, SupportedLocale::ZhCn),
            (r#""en-US""#, SupportedLocale::EnUs),
        ] {
            let decoded: SupportedLocale = serde_json::from_str(json).unwrap();
            assert_eq!(decoded, value);
            assert_eq!(serde_json::to_string(&decoded).unwrap(), json);
        }
    }

    #[tokio::test]
    async fn userscript_secret_round_trips_through_the_credential_store() {
        let (store, root, credentials) = test_store().await;

        // 启用共享密钥：自动生成，写入凭据库，settings.json 保持明文为零。
        let mut value = store.get().await;
        value.secret_enabled = true;
        value.secret = None;
        let updated = store.update(value).await.unwrap();
        let generated = updated.secret.clone().expect("a secret must be generated");
        assert!(updated.secret_enabled);
        assert_settings_json_has_no_secret(&root).await;
        let stored = credentials
            .get(USERSCRIPT_SECRET_KEY, SecretKind::UserscriptSecret)
            .await
            .unwrap()
            .expect("the generated secret must live in the credential store");
        assert_eq!(stored.expose_secret(), generated);

        // 重新加载：运行时密钥从凭据库恢复，展示/校验命令仍能取到值。
        drop(store);
        let reloaded = SettingsStore::load_with_credential_store(
            root.join("settings.json"),
            credentials.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            reloaded.get().await.secret.as_deref(),
            Some(generated.as_str())
        );

        // 轮换：新值写凭据库，settings.json 依旧无明文。
        let rotated = reloaded.rotate_secret().await.unwrap();
        let rotated_secret = rotated.secret.clone().unwrap();
        assert_ne!(rotated_secret, generated);
        assert_settings_json_has_no_secret(&root).await;
        assert_eq!(
            credentials
                .get(USERSCRIPT_SECRET_KEY, SecretKind::UserscriptSecret)
                .await
                .unwrap()
                .unwrap()
                .expose_secret(),
            rotated_secret
        );

        // 关闭共享密钥：凭据库条目一并移除。
        let mut value = reloaded.get().await;
        value.secret_enabled = false;
        let disabled = reloaded.update(value).await.unwrap();
        assert!(disabled.secret.is_none());
        assert!(
            credentials
                .get(USERSCRIPT_SECRET_KEY, SecretKind::UserscriptSecret)
                .await
                .unwrap()
                .is_none()
        );

        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn legacy_plaintext_secret_is_migrated_once_into_the_credential_store() {
        let root = std::env::temp_dir().join(format!(
            "acm-settings-cfg4-migrate-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let credentials = Arc::new(MemoryCredentialStore::default());
        tokio::fs::write(
            root.join("settings.json"),
            r#"{"setup_complete":true,"secret_enabled":true,"secret":"legacy-plaintext-secret","allowed_origins":[]}"#,
        )
        .await
        .unwrap();

        let store = SettingsStore::load_with_credential_store(
            root.join("settings.json"),
            credentials.clone(),
        )
        .await
        .unwrap();

        // 明文被一次性迁入凭据库，并从 settings.json 移除。
        assert_eq!(
            credentials
                .get(USERSCRIPT_SECRET_KEY, SecretKind::UserscriptSecret)
                .await
                .unwrap()
                .unwrap()
                .expose_secret(),
            "legacy-plaintext-secret"
        );
        assert_settings_json_has_no_secret(&root).await;
        // 运行时密钥仍可用，userscript/MCP 授权不受影响。
        assert_eq!(
            store.get().await.secret.as_deref(),
            Some("legacy-plaintext-secret")
        );

        // 再次加载：无需迁移，密钥仍从凭据库恢复。
        drop(store);
        let reloaded = SettingsStore::load_with_credential_store(
            root.join("settings.json"),
            credentials.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            reloaded.get().await.secret.as_deref(),
            Some("legacy-plaintext-secret")
        );

        let _ = tokio::fs::remove_dir_all(root).await;
    }
}
