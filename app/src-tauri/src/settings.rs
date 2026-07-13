use rand::RngCore;
use std::path::PathBuf;
use tokio::sync::RwLock;

use crate::{
    error::{AppError, Result},
    models::AppSettings,
};

pub struct SettingsStore {
    path: PathBuf,
    value: RwLock<AppSettings>,
}

impl SettingsStore {
    pub async fn load(path: PathBuf) -> Result<Self> {
        let value = if path.exists() {
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
        Ok(Self {
            path,
            value: RwLock::new(value),
        })
    }
    pub async fn get(&self) -> AppSettings {
        self.value.read().await.clone()
    }
    pub async fn update(&self, mut value: AppSettings) -> Result<AppSettings> {
        validate_origins(&value.allowed_origins)?;
        if value.secret_enabled && value.secret.as_deref().is_none_or(str::is_empty) {
            value.secret = Some(generate_secret());
        }
        if !value.secret_enabled {
            value.secret = None;
        }
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let temporary = self.path.with_extension("json.tmp");
        let backup = self.path.with_extension("json.bak");
        tokio::fs::write(&temporary, serde_json::to_vec_pretty(&value)?).await?;
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
        *self.value.write().await = value.clone();
        Ok(value)
    }
    pub async fn rotate_secret(&self) -> Result<AppSettings> {
        let mut value = self.get().await;
        value.secret_enabled = true;
        value.secret = Some(generate_secret());
        self.update(value).await
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
            r#"{"setup_complete":true,"secret_enabled":false,"secret":null,"allowed_origins":[],"migrated_legacy_database":false}"#,
        )
        .unwrap();
        assert_eq!(settings.close_behavior, crate::models::CloseBehavior::Ask);
        assert_eq!(
            settings.tray_click_behavior,
            crate::models::TrayClickBehavior::ShowMenu
        );
        assert!(settings.data_directory.is_none());
        assert_eq!(settings.theme, crate::models::ThemePreference::System);
    }

    #[test]
    fn disables_secret_validation_by_default() {
        let settings = AppSettings::default();
        assert!(!settings.secret_enabled);
        assert!(settings.secret.is_none());
        assert_eq!(settings.theme, crate::models::ThemePreference::System);
    }
}
