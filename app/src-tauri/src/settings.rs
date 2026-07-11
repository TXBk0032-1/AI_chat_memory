use rand::RngCore;
use std::path::{Path, PathBuf};
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
            serde_json::from_slice(&tokio::fs::read(&path).await?)?
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
        tokio::fs::write(&self.path, serde_json::to_vec_pretty(&value)?).await?;
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

pub async fn migrate_legacy_database(
    legacy: &Path,
    target: &Path,
    store: &SettingsStore,
) -> Result<bool> {
    let mut settings = store.get().await;
    if settings.migrated_legacy_database || target.exists() || !legacy.exists() {
        return Ok(false);
    }
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let temp = target.with_extension("db.migrating");
    tokio::fs::copy(legacy, &temp).await?;
    tokio::fs::rename(&temp, target).await?;
    settings.migrated_legacy_database = true;
    store.update(settings).await?;
    Ok(true)
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
    async fn migrates_once_without_touching_source() {
        let root = std::env::temp_dir().join(format!("acm-test-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&root).await.unwrap();
        let legacy = root.join("legacy.db");
        let target = root.join("data/app.db");
        tokio::fs::write(&legacy, b"sqlite-data").await.unwrap();
        let store = SettingsStore::load(root.join("settings.json"))
            .await
            .unwrap();
        assert!(
            migrate_legacy_database(&legacy, &target, &store)
                .await
                .unwrap()
        );
        assert_eq!(tokio::fs::read(&legacy).await.unwrap(), b"sqlite-data");
        assert!(
            !migrate_legacy_database(&legacy, &target, &store)
                .await
                .unwrap()
        );
        let _ = tokio::fs::remove_dir_all(root).await;
    }
}
