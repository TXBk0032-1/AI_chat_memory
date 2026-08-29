use crate::{
    error::{AppError, Result},
    models::{CloudBackendKind, CloudCredentialInput, CloudSyncSettings},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fmt, sync::Arc};
use tokio::sync::Mutex;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

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
    S3AccessKeyId,
    S3SecretAccessKey,
    S3SessionToken,
    SyncPassphrase,
    CloudCredentialBundle,
    UserscriptSecret,
}

impl SecretKind {
    fn key(self) -> &'static str {
        match self {
            Self::WebDavPassword => "webdav-password",
            Self::S3AccessKeyId => "s3-access-key-id",
            Self::S3SecretAccessKey => "s3-secret-access-key",
            Self::S3SessionToken => "s3-session-token",
            Self::SyncPassphrase => "sync-passphrase",
            Self::CloudCredentialBundle => "cloud-credential-bundle-v1",
            Self::UserscriptSecret => "userscript-secret",
        }
    }
}

pub const CREDENTIAL_BUNDLE_VERSION: u8 = 1;
const SYSTEM_SECRET_V1_PREFIX: &[u8] = b"ai-chat-memory-secret-v1\0";
const WINDOWS_CREDENTIAL_BLOB_MAX_BYTES: usize = 2_560;
const MAX_CREDENTIAL_BUNDLE_BYTES: usize =
    WINDOWS_CREDENTIAL_BLOB_MAX_BYTES - SYSTEM_SECRET_V1_PREFIX.len();
const LEGACY_SECRET_KINDS: [SecretKind; 5] = [
    SecretKind::WebDavPassword,
    SecretKind::S3AccessKeyId,
    SecretKind::S3SecretAccessKey,
    SecretKind::S3SessionToken,
    SecretKind::SyncPassphrase,
];

#[cfg(windows)]
static SYSTEM_CREDENTIAL_STORE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(windows)]
fn with_system_credential_store_lock<T>(operation: impl FnOnce() -> T) -> T {
    let _guard = SYSTEM_CREDENTIAL_STORE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    operation()
}

#[cfg(not(windows))]
fn with_system_credential_store_lock<T>(operation: impl FnOnce() -> T) -> T {
    operation()
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum StoredCloudCredentialProfile {
    Webdav {
        password: String,
        sync_passphrase: Option<String>,
    },
    S3 {
        access_key_id: String,
        secret_access_key: String,
        session_token: Option<String>,
        sync_passphrase: Option<String>,
    },
}

impl StoredCloudCredentialProfile {
    pub fn from_input(input: &CloudCredentialInput) -> Self {
        match input {
            CloudCredentialInput::Webdav {
                password,
                sync_password,
            } => Self::Webdav {
                password: password.clone(),
                sync_passphrase: sync_password.clone().filter(|value| !value.is_empty()),
            },
            CloudCredentialInput::S3 {
                access_key_id,
                secret_access_key,
                session_token,
                sync_password,
            } => Self::S3 {
                access_key_id: access_key_id.clone(),
                secret_access_key: secret_access_key.clone(),
                session_token: session_token.clone().filter(|value| !value.is_empty()),
                sync_passphrase: sync_password.clone().filter(|value| !value.is_empty()),
            },
        }
    }

    pub fn backend(&self) -> CloudBackendKind {
        match self {
            Self::Webdav { .. } => CloudBackendKind::Webdav,
            Self::S3 { .. } => CloudBackendKind::S3,
        }
    }

    pub fn sync_passphrase(&self) -> Option<&str> {
        match self {
            Self::Webdav {
                sync_passphrase, ..
            }
            | Self::S3 {
                sync_passphrase, ..
            } => sync_passphrase.as_deref(),
        }
    }
}

impl fmt::Debug for StoredCloudCredentialProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct(match self {
                Self::Webdav { .. } => "StoredWebdavCredentialProfile",
                Self::S3 { .. } => "StoredS3CredentialProfile",
            })
            .field("credentials", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Zeroize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialTransitionPhase {
    Prepared,
    RemoteFrozen,
    RemoteCommitted,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct PendingCredentialProfile {
    pub credentials: StoredCloudCredentialProfile,
    pub operation_id: String,
    pub target_vault_id: String,
    pub target_generation_id: String,
    pub phase: CredentialTransitionPhase,
}

impl fmt::Debug for PendingCredentialProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingCredentialProfile")
            .field("credentials", &"[REDACTED]")
            .field("operation_id", &self.operation_id)
            .field("target_vault_id", &self.target_vault_id)
            .field("target_generation_id", &self.target_generation_id)
            .field("phase", &self.phase)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct StoredCredentialBundle {
    pub version: u8,
    pub active: StoredCloudCredentialProfile,
    pub pending: Option<PendingCredentialProfile>,
}

impl fmt::Debug for StoredCredentialBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredCredentialBundle")
            .field("version", &self.version)
            .field("active", &"[REDACTED]")
            .field("pending", &self.pending)
            .finish()
    }
}

impl StoredCredentialBundle {
    pub fn new(active: StoredCloudCredentialProfile) -> Self {
        Self {
            version: CREDENTIAL_BUNDLE_VERSION,
            active,
            pending: None,
        }
    }

    pub fn stage_transition(&mut self, pending: PendingCredentialProfile) -> Result<()> {
        if self.pending.is_some()
            || pending.operation_id.is_empty()
            || pending.target_vault_id.is_empty()
            || pending.target_generation_id.is_empty()
            || pending.credentials.backend() != self.active.backend()
        {
            return Err(AppError::Credential(
                "cloud credential transition is invalid".into(),
            ));
        }
        self.pending = Some(pending);
        Ok(())
    }

    pub fn set_pending_phase(
        &mut self,
        operation_id: &str,
        phase: CredentialTransitionPhase,
    ) -> Result<()> {
        let pending = self
            .pending
            .as_mut()
            .ok_or_else(|| AppError::Credential("cloud credential transition is missing".into()))?;
        if pending.operation_id != operation_id {
            return Err(AppError::Credential(
                "cloud credential transition operation does not match".into(),
            ));
        }
        if phase < pending.phase {
            return Err(AppError::Credential(
                "cloud credential transition phase cannot move backwards".into(),
            ));
        }
        pending.phase = phase;
        Ok(())
    }

    pub fn promote_pending(&mut self, operation_id: &str) -> Result<()> {
        let pending = self
            .pending
            .as_ref()
            .ok_or_else(|| AppError::Credential("cloud credential transition is missing".into()))?;
        if pending.operation_id != operation_id {
            return Err(AppError::Credential(
                "cloud credential transition operation does not match".into(),
            ));
        }
        if pending.phase != CredentialTransitionPhase::RemoteCommitted {
            return Err(AppError::Credential(
                "cloud credential transition has not committed remotely".into(),
            ));
        }
        self.active = pending.credentials.clone();
        self.pending = None;
        Ok(())
    }

    pub fn discard_pending(&mut self, operation_id: &str) -> Result<()> {
        let pending = self
            .pending
            .as_ref()
            .ok_or_else(|| AppError::Credential("cloud credential transition is missing".into()))?;
        if pending.operation_id != operation_id {
            return Err(AppError::Credential(
                "cloud credential transition operation does not match".into(),
            ));
        }
        self.pending = None;
        Ok(())
    }
}

pub async fn load_credential_bundle<S: CredentialStore + ?Sized>(
    store: &S,
    remote_id: &str,
) -> Result<Option<StoredCredentialBundle>> {
    let Some(secret) = store
        .get(remote_id, SecretKind::CloudCredentialBundle)
        .await?
    else {
        return Ok(None);
    };
    let encoded = secret.expose_secret();
    if encoded.len() > MAX_CREDENTIAL_BUNDLE_BYTES {
        return Err(AppError::Credential(
            "stored cloud credential bundle exceeds its size limit".into(),
        ));
    }
    let bundle: StoredCredentialBundle = serde_json::from_str(encoded)
        .map_err(|_| AppError::Credential("stored cloud credential bundle is invalid".into()))?;
    if bundle.version != CREDENTIAL_BUNDLE_VERSION {
        return Err(AppError::Credential(
            "stored cloud credential bundle version is unsupported".into(),
        ));
    }
    Ok(Some(bundle))
}

pub async fn save_credential_bundle<S: CredentialStore + ?Sized>(
    store: &S,
    remote_id: &str,
    bundle: &StoredCredentialBundle,
) -> Result<()> {
    if bundle.version != CREDENTIAL_BUNDLE_VERSION {
        return Err(AppError::Credential(
            "cloud credential bundle version is unsupported".into(),
        ));
    }
    let encoded = serde_json::to_string(bundle)
        .map_err(|_| AppError::Credential("cloud credential bundle encoding failed".into()))?;
    if encoded.len() > MAX_CREDENTIAL_BUNDLE_BYTES {
        return Err(AppError::Credential(
            "cloud credential bundle exceeds its size limit".into(),
        ));
    }
    store
        .set(
            remote_id,
            SecretKind::CloudCredentialBundle,
            SecretValue::new(encoded),
        )
        .await
}

pub async fn delete_credential_bundle<S: CredentialStore + ?Sized>(
    store: &S,
    remote_id: &str,
) -> Result<()> {
    store
        .delete(remote_id, SecretKind::CloudCredentialBundle)
        .await
}

pub async fn load_or_migrate_credential_bundle<S: CredentialStore + ?Sized>(
    store: &S,
    settings: &CloudSyncSettings,
) -> Result<Option<StoredCredentialBundle>> {
    if let Some(bundle) = load_credential_bundle(store, &settings.remote_id).await? {
        if bundle.active.backend() != settings.backend {
            return Err(AppError::Credential(
                "stored cloud credentials do not match the selected backend".into(),
            ));
        }
        return Ok(Some(bundle));
    }

    let active = match settings.backend {
        CloudBackendKind::Webdav => {
            let Some(password) = store
                .get(&settings.remote_id, SecretKind::WebDavPassword)
                .await?
            else {
                return Ok(None);
            };
            let sync_passphrase = store
                .get(&settings.remote_id, SecretKind::SyncPassphrase)
                .await?
                .map(|value| value.expose_secret().to_owned());
            StoredCloudCredentialProfile::Webdav {
                password: password.expose_secret().to_owned(),
                sync_passphrase,
            }
        }
        CloudBackendKind::S3 => {
            let Some(access_key_id) = store
                .get(&settings.remote_id, SecretKind::S3AccessKeyId)
                .await?
            else {
                return Ok(None);
            };
            let Some(secret_access_key) = store
                .get(&settings.remote_id, SecretKind::S3SecretAccessKey)
                .await?
            else {
                return Ok(None);
            };
            let session_token = store
                .get(&settings.remote_id, SecretKind::S3SessionToken)
                .await?
                .map(|value| value.expose_secret().to_owned());
            let sync_passphrase = store
                .get(&settings.remote_id, SecretKind::SyncPassphrase)
                .await?
                .map(|value| value.expose_secret().to_owned());
            StoredCloudCredentialProfile::S3 {
                access_key_id: access_key_id.expose_secret().to_owned(),
                secret_access_key: secret_access_key.expose_secret().to_owned(),
                session_token,
                sync_passphrase,
            }
        }
    };
    let bundle = StoredCredentialBundle::new(active);
    save_credential_bundle(store, &settings.remote_id, &bundle).await?;
    for kind in LEGACY_SECRET_KINDS {
        if let Err(error) = store.delete(&settings.remote_id, kind).await {
            tracing::warn!(?kind, %error, "legacy cloud credential cleanup failed");
        }
    }
    Ok(Some(bundle))
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
            with_system_credential_store_lock(|| {
                let entry = keyring::Entry::new(&service, &account)
                    .map_err(|_| AppError::Credential("credential backend unavailable".into()))?;
                match entry.get_secret() {
                    Ok(value) if value.starts_with(SYSTEM_SECRET_V1_PREFIX) => {
                        let value =
                            String::from_utf8(value[SYSTEM_SECRET_V1_PREFIX.len()..].to_vec())
                                .map_err(|_| {
                                    AppError::Credential(
                                        "stored credential encoding is invalid".into(),
                                    )
                                })?;
                        Ok(Some(SecretValue::new(value)))
                    }
                    Ok(_) | Err(_) => match entry.get_password() {
                        Ok(value) => Ok(Some(SecretValue::new(value))),
                        Err(keyring::Error::NoEntry) => Ok(None),
                        Err(_) => Err(AppError::Credential("credential read failed".into())),
                    },
                }
            })
        })
        .await
        .map_err(|_| AppError::Credential("credential read task failed".into()))?
    }

    async fn set(&self, vault_key: &str, kind: SecretKind, value: SecretValue) -> Result<()> {
        let service = self.service.to_string();
        let account = Self::account(vault_key, kind)?;
        tokio::task::spawn_blocking(move || {
            with_system_credential_store_lock(|| {
                let entry = keyring::Entry::new(&service, &account)
                    .map_err(|_| AppError::Credential("credential backend unavailable".into()))?;
                let mut encoded =
                    Vec::with_capacity(SYSTEM_SECRET_V1_PREFIX.len() + value.expose_secret().len());
                encoded.extend_from_slice(SYSTEM_SECRET_V1_PREFIX);
                encoded.extend_from_slice(value.expose_secret().as_bytes());
                if encoded.len() > WINDOWS_CREDENTIAL_BLOB_MAX_BYTES {
                    return Err(AppError::Credential(
                        "credential exceeds the Windows credential storage limit".into(),
                    ));
                }
                entry
                    .set_secret(&encoded)
                    .map_err(|_| AppError::Credential("credential write failed".into()))
            })
        })
        .await
        .map_err(|_| AppError::Credential("credential write task failed".into()))?
    }

    async fn delete(&self, vault_key: &str, kind: SecretKind) -> Result<()> {
        let service = self.service.to_string();
        let account = Self::account(vault_key, kind)?;
        tokio::task::spawn_blocking(move || {
            with_system_credential_store_lock(|| {
                let entry = keyring::Entry::new(&service, &account)
                    .map_err(|_| AppError::Credential("credential backend unavailable".into()))?;
                match entry.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                    Err(_) => Err(AppError::Credential("credential delete failed".into())),
                }
            })
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

    #[cfg(windows)]
    struct SystemCredentialCleanup {
        service: String,
        account: String,
    }

    #[cfg(windows)]
    impl Drop for SystemCredentialCleanup {
        fn drop(&mut self) {
            with_system_credential_store_lock(|| {
                if let Ok(entry) = keyring::Entry::new(&self.service, &self.account) {
                    let _ = entry.delete_credential();
                }
            });
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn system_store_round_trips_across_entry_instances_and_cleans_up() {
        let service = format!("ai-chat-memory-test-{}", uuid::Uuid::new_v4());
        let vault_key = format!("vault-{}", uuid::Uuid::new_v4());
        let kind = SecretKind::S3SecretAccessKey;
        let account = SystemCredentialStore::account(&vault_key, kind).unwrap();
        let _cleanup = SystemCredentialCleanup {
            service: service.clone(),
            account,
        };

        let first = SystemCredentialStore::new(&service);
        first.delete(&vault_key, kind).await.unwrap();
        first
            .set(&vault_key, kind, SecretValue::new("cross-entry-secret"))
            .await
            .unwrap();

        let second = SystemCredentialStore::new(&service);
        let loaded = second.get(&vault_key, kind).await.unwrap().unwrap();
        assert_eq!(loaded.expose_secret(), "cross-entry-secret");

        let third = SystemCredentialStore::new(&service);
        third.delete(&vault_key, kind).await.unwrap();

        let fourth = SystemCredentialStore::new(&service);
        assert!(fourth.get(&vault_key, kind).await.unwrap().is_none());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn system_store_reads_legacy_password_encoded_credentials() {
        let service = format!("ai-chat-memory-test-{}", uuid::Uuid::new_v4());
        let vault_key = format!("vault-{}", uuid::Uuid::new_v4());
        let kind = SecretKind::S3SessionToken;
        let account = SystemCredentialStore::account(&vault_key, kind).unwrap();
        let _cleanup = SystemCredentialCleanup {
            service: service.clone(),
            account: account.clone(),
        };
        with_system_credential_store_lock(|| {
            keyring::Entry::new(&service, &account)
                .unwrap()
                .set_password("legacy-session-token")
                .unwrap();
        });

        let loaded = SystemCredentialStore::new(&service)
            .get(&vault_key, kind)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(loaded.expose_secret(), "legacy-session-token");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn system_store_persists_a_long_atomic_s3_transition_across_restarts() {
        let service = format!("ai-chat-memory-test-{}", uuid::Uuid::new_v4());
        let remote_id = format!("remote-{}", uuid::Uuid::new_v4());
        let kind = SecretKind::CloudCredentialBundle;
        let account = SystemCredentialStore::account(&remote_id, kind).unwrap();
        let _cleanup = SystemCredentialCleanup {
            service: service.clone(),
            account,
        };
        let session_token = "T".repeat(375);
        let mut bundle = StoredCredentialBundle {
            version: CREDENTIAL_BUNDLE_VERSION,
            active: StoredCloudCredentialProfile::S3 {
                access_key_id: "A".repeat(20),
                secret_access_key: "S".repeat(40),
                session_token: Some(session_token.clone()),
                sync_passphrase: Some("old-sync-passphrase".into()),
            },
            pending: Some(PendingCredentialProfile {
                credentials: StoredCloudCredentialProfile::S3 {
                    access_key_id: "B".repeat(20),
                    secret_access_key: "N".repeat(40),
                    session_token: Some(session_token),
                    sync_passphrase: Some("new-sync-passphrase".into()),
                },
                operation_id: "rotation-windows-keyring-boundary".into(),
                target_vault_id: "vault-next".into(),
                target_generation_id: "generation-next".into(),
                phase: CredentialTransitionPhase::Prepared,
            }),
        };
        let encoded = serde_json::to_string(&bundle).unwrap();
        assert!(encoded.encode_utf16().count() * 2 > 2_560);
        assert!(encoded.len() < 2_560);

        save_credential_bundle(&SystemCredentialStore::new(&service), &remote_id, &bundle)
            .await
            .unwrap();
        let reloaded = load_credential_bundle(&SystemCredentialStore::new(&service), &remote_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reloaded, bundle);

        bundle
            .set_pending_phase(
                "rotation-windows-keyring-boundary",
                CredentialTransitionPhase::RemoteCommitted,
            )
            .unwrap();
        save_credential_bundle(&SystemCredentialStore::new(&service), &remote_id, &bundle)
            .await
            .unwrap();
        let mut reloaded =
            load_credential_bundle(&SystemCredentialStore::new(&service), &remote_id)
                .await
                .unwrap()
                .unwrap();
        reloaded
            .promote_pending("rotation-windows-keyring-boundary")
            .unwrap();
        save_credential_bundle(&SystemCredentialStore::new(&service), &remote_id, &reloaded)
            .await
            .unwrap();

        let promoted = load_credential_bundle(&SystemCredentialStore::new(&service), &remote_id)
            .await
            .unwrap()
            .unwrap();
        assert!(promoted.pending.is_none());
        assert_eq!(
            promoted.active.sync_passphrase(),
            Some("new-sync-passphrase")
        );
    }

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

    #[tokio::test]
    async fn memory_store_keeps_s3_credentials_separate_and_redacted() {
        let store = MemoryCredentialStore::default();
        let credentials = [
            (SecretKind::S3AccessKeyId, "AKID"),
            (SecretKind::S3SecretAccessKey, "secret-key"),
            (SecretKind::S3SessionToken, "session-token"),
        ];

        for (kind, value) in credentials {
            let secret = SecretValue::new(value);
            assert!(!format!("{secret:?}").contains(value));
            store.set("vault-s3", kind, secret).await.unwrap();
        }

        for (kind, value) in credentials {
            let loaded = store.get("vault-s3", kind).await.unwrap().unwrap();
            assert_eq!(loaded.expose_secret(), value);
            store.delete("vault-s3", kind).await.unwrap();
            assert!(store.get("vault-s3", kind).await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn versioned_cloud_credentials_round_trip_as_one_redacted_entry() {
        let store = MemoryCredentialStore::default();
        let bundle = StoredCredentialBundle {
            version: CREDENTIAL_BUNDLE_VERSION,
            active: StoredCloudCredentialProfile::S3 {
                access_key_id: "OLD-AKID".into(),
                secret_access_key: "old-secret".into(),
                session_token: Some("old-token".into()),
                sync_passphrase: Some("old-passphrase".into()),
            },
            pending: Some(PendingCredentialProfile {
                credentials: StoredCloudCredentialProfile::S3 {
                    access_key_id: "NEW-AKID".into(),
                    secret_access_key: "new-secret".into(),
                    session_token: None,
                    sync_passphrase: Some("new-passphrase".into()),
                },
                operation_id: "rotation-1".into(),
                target_vault_id: "vault-1".into(),
                target_generation_id: "generation-2".into(),
                phase: CredentialTransitionPhase::Prepared,
            }),
        };

        save_credential_bundle(&store, "remote-1", &bundle)
            .await
            .unwrap();

        for kind in [
            SecretKind::S3AccessKeyId,
            SecretKind::S3SecretAccessKey,
            SecretKind::S3SessionToken,
            SecretKind::SyncPassphrase,
        ] {
            assert!(store.get("remote-1", kind).await.unwrap().is_none());
        }
        let loaded = load_credential_bundle(&store, "remote-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded, bundle);
        let debug = format!("{loaded:?}");
        for secret in [
            "OLD-AKID",
            "old-secret",
            "old-token",
            "old-passphrase",
            "NEW-AKID",
            "new-secret",
            "new-passphrase",
        ] {
            assert!(!debug.contains(secret));
        }
    }

    #[test]
    fn credential_transition_keeps_both_profiles_until_the_expected_operation_is_promoted() {
        let active = StoredCloudCredentialProfile::Webdav {
            password: "old-password".into(),
            sync_passphrase: Some("old-passphrase".into()),
        };
        let pending = StoredCloudCredentialProfile::Webdav {
            password: "new-password".into(),
            sync_passphrase: Some("new-passphrase".into()),
        };
        let mut bundle = StoredCredentialBundle::new(active.clone());

        bundle
            .stage_transition(PendingCredentialProfile {
                credentials: pending.clone(),
                operation_id: "rotation-expected".into(),
                target_vault_id: "vault-1".into(),
                target_generation_id: "generation-2".into(),
                phase: CredentialTransitionPhase::Prepared,
            })
            .unwrap();
        assert_eq!(bundle.active, active);
        assert_eq!(bundle.pending.as_ref().unwrap().credentials, pending);

        assert!(bundle.promote_pending("rotation-other").is_err());
        assert_eq!(bundle.active, active);
        assert!(bundle.pending.is_some());
        assert!(bundle.promote_pending("rotation-expected").is_err());
        assert_eq!(bundle.active, active);
        assert!(bundle.pending.is_some());

        bundle
            .set_pending_phase(
                "rotation-expected",
                CredentialTransitionPhase::RemoteCommitted,
            )
            .unwrap();
        bundle.promote_pending("rotation-expected").unwrap();
        assert_eq!(bundle.active, pending);
        assert!(bundle.pending.is_none());
    }

    #[test]
    fn credential_transition_phase_only_moves_forward() {
        let active = StoredCloudCredentialProfile::Webdav {
            password: "old-password".into(),
            sync_passphrase: None,
        };
        let mut bundle = StoredCredentialBundle::new(active.clone());
        bundle
            .stage_transition(PendingCredentialProfile {
                credentials: active,
                operation_id: "rotation-forward-only".into(),
                target_vault_id: "vault-1".into(),
                target_generation_id: "generation-2".into(),
                phase: CredentialTransitionPhase::Prepared,
            })
            .unwrap();

        bundle
            .set_pending_phase(
                "rotation-forward-only",
                CredentialTransitionPhase::RemoteFrozen,
            )
            .unwrap();
        assert!(
            bundle
                .set_pending_phase("rotation-forward-only", CredentialTransitionPhase::Prepared,)
                .is_err()
        );
        bundle
            .set_pending_phase(
                "rotation-forward-only",
                CredentialTransitionPhase::RemoteCommitted,
            )
            .unwrap();
        assert!(
            bundle
                .set_pending_phase(
                    "rotation-forward-only",
                    CredentialTransitionPhase::RemoteFrozen,
                )
                .is_err()
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
