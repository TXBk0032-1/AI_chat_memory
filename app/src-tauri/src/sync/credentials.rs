use crate::error::{AppError, Result};
use async_trait::async_trait;
use serde::Serialize;
use std::{collections::HashMap, fmt, sync::Arc};
use tokio::sync::Mutex;
use zeroize::Zeroizing;

type CredentialKey = (String, SecretKind);
type CredentialValues = HashMap<CredentialKey, Zeroizing<String>>;

pub struct SecretValue(pub Zeroizing<String>);

impl SecretValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    pub fn expose_secret(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue([REDACTED])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretKind {
    WebDavPassword,
    SyncPassphrase,
}

impl SecretKind {
    fn key(self) -> &'static str {
        match self {
            Self::WebDavPassword => "webdav-password",
            Self::SyncPassphrase => "sync-passphrase",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CredentialLocation {
    pub vault_key: String,
}

impl CredentialLocation {
    pub fn new(vault_key: impl Into<String>) -> Self {
        Self {
            vault_key: vault_key.into(),
        }
    }
}

#[async_trait]
pub trait CredentialStore: Send + Sync {
    async fn get(&self, vault_key: &str, kind: SecretKind) -> Result<Option<SecretValue>>;
    async fn set(&self, vault_key: &str, kind: SecretKind, value: SecretValue) -> Result<()>;
    async fn delete(&self, vault_key: &str, kind: SecretKind) -> Result<()>;
}

#[derive(Clone)]
pub struct SystemCredentialStore {
    service: Arc<str>,
}

impl SystemCredentialStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: Arc::from(service.into()),
        }
    }

    fn account(vault_key: &str, kind: SecretKind) -> Result<String> {
        if vault_key.is_empty() || vault_key.contains(':') {
            return Err(AppError::Credential("invalid vault credential key".into()));
        }
        Ok(format!("{vault_key}:{}", kind.key()))
    }
}

#[async_trait]
impl CredentialStore for SystemCredentialStore {
    async fn get(&self, vault_key: &str, kind: SecretKind) -> Result<Option<SecretValue>> {
        let service = self.service.to_string();
        let account = Self::account(vault_key, kind)?;
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(&service, &account)
                .map_err(|_| AppError::Credential("credential backend unavailable".into()))?;
            match entry.get_password() {
                Ok(value) => Ok(Some(SecretValue::new(value))),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(_) => Err(AppError::Credential("credential read failed".into())),
            }
        })
        .await
        .map_err(|_| AppError::Credential("credential read task failed".into()))?
    }

    async fn set(&self, vault_key: &str, kind: SecretKind, value: SecretValue) -> Result<()> {
        let service = self.service.to_string();
        let account = Self::account(vault_key, kind)?;
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(&service, &account)
                .map_err(|_| AppError::Credential("credential backend unavailable".into()))?;
            entry
                .set_password(value.expose_secret())
                .map_err(|_| AppError::Credential("credential write failed".into()))
        })
        .await
        .map_err(|_| AppError::Credential("credential write task failed".into()))?
    }

    async fn delete(&self, vault_key: &str, kind: SecretKind) -> Result<()> {
        let service = self.service.to_string();
        let account = Self::account(vault_key, kind)?;
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(&service, &account)
                .map_err(|_| AppError::Credential("credential backend unavailable".into()))?;
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(_) => Err(AppError::Credential("credential delete failed".into())),
            }
        })
        .await
        .map_err(|_| AppError::Credential("credential delete task failed".into()))?
    }
}

#[derive(Default, Clone)]
pub struct MemoryCredentialStore {
    values: Arc<Mutex<CredentialValues>>,
}

#[async_trait]
impl CredentialStore for MemoryCredentialStore {
    async fn get(&self, vault_key: &str, kind: SecretKind) -> Result<Option<SecretValue>> {
        Ok(self
            .values
            .lock()
            .await
            .get(&(vault_key.to_owned(), kind))
            .cloned()
            .map(SecretValue))
    }

    async fn set(&self, vault_key: &str, kind: SecretKind, value: SecretValue) -> Result<()> {
        self.values
            .lock()
            .await
            .insert((vault_key.to_owned(), kind), value.0);
        Ok(())
    }

    async fn delete(&self, vault_key: &str, kind: SecretKind) -> Result<()> {
        self.values
            .lock()
            .await
            .remove(&(vault_key.to_owned(), kind));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_store_round_trips_and_deletes_redacted_secrets() {
        let store = MemoryCredentialStore::default();
        let secret = SecretValue::new("webdav-password");
        assert!(!format!("{secret:?}").contains("webdav-password"));

        store
            .set("vault-a", SecretKind::WebDavPassword, secret)
            .await
            .unwrap();
        let loaded = store
            .get("vault-a", SecretKind::WebDavPassword)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.expose_secret(), "webdav-password");
        store
            .delete("vault-a", SecretKind::WebDavPassword)
            .await
            .unwrap();
        assert!(
            store
                .get("vault-a", SecretKind::WebDavPassword)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn secret_values_are_not_serializable_settings_fields() {
        assert_eq!(
            serde_json::to_string(&CredentialLocation::new("vault-a")).unwrap(),
            r#"{"vault_key":"vault-a"}"#
        );
    }
}
