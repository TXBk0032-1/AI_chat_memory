use rand::RngCore;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use tokio::io::AsyncWriteExt;

use crate::{
    error::{AppError, Result},
    models::AppSettings,
    sync::credentials::{CredentialStore, SecretKind, SecretValue, SystemCredentialStore},
};

/// The userscript shared secret lives in the system credential store
/// under this fixed vault key, never in settings.json.
const USERSCRIPT_SECRET_KEY: &str = "userscript";
const CREDENTIAL_SERVICE: &str = "ai-chat-memory";
/// A `settings.json.tmp-*` file younger than this may still belong to a live
/// writer in another process (desktop app vs. MCP child), so cleanup leaves it
/// alone. Only leftovers from crashed writers are old enough to be removed.
const STALE_TEMPORARY_AGE: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// How `persist_with_policy` treats `settings.json.bak` before replacing main.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackupPolicy {
    /// The current main was parsed successfully by this process: copy it to
    /// `.bak` so the one-version-older recovery point moves forward.
    RotateValidatedMain,
    /// The current main is known to be corrupt (we are self-healing from
    /// `.bak`): never touch the backup, it is the only valid copy on disk.
    PreserveExisting,
}

pub struct SettingsStore {
    path: PathBuf,
    value: RwLock<AppSettings>,
    credentials: Arc<dyn CredentialStore>,
    /// Serializes the whole validate → persist → in-memory swap chain in
    /// `update`, so concurrent writers cannot interleave on the same tmp and
    /// backup file names. `persist` itself must be called while holding it
    /// (or during single-threaded load-time repair).
    write_lock: tokio::sync::Mutex<()>,
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
        let mut recovered_from_backup = false;
        let mut value = if path.exists() {
            match serde_json::from_slice(&tokio::fs::read(&path).await?) {
                Ok(value) => value,
                Err(error) => match Self::read_backup(&path).await {
                    Some(backup_value) => {
                        tracing::warn!(
                            %error,
                            "settings.json was corrupt; recovered from settings.json.bak and rewriting it"
                        );
                        recovered_from_backup = true;
                        backup_value
                    }
                    None => {
                        let corrupt = path.with_extension(format!(
                            "corrupt-{}.json",
                            chrono::Utc::now().timestamp()
                        ));
                        tokio::fs::rename(&path, &corrupt).await?;
                        tracing::error!(
                            %error,
                            path=%corrupt.display(),
                            "settings were corrupt and no backup could be read; restored defaults"
                        );
                        AppSettings::default()
                    }
                },
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
        // A legacy settings.json may still carry the plaintext userscript
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
            write_lock: tokio::sync::Mutex::new(()),
        };
        // Every write-back performed by this load shares one backup policy:
        // once main was found corrupt, `.bak` is the only valid copy and must
        // survive untouched, whatever else this load needs to rewrite.
        let policy = if recovered_from_backup {
            BackupPolicy::PreserveExisting
        } else {
            BackupPolicy::RotateValidatedMain
        };
        if migrated_plaintext_secret {
            if let Err(error) = store.persist_with_policy(&value, policy).await {
                tracing::warn!(
                    %error,
                    "failed to remove the plaintext userscript secret from settings.json"
                );
            }
            if recovered_from_backup {
                // 权衡：备份含明文密钥，但它也是磁盘上唯一合法的配置。
                // 配置可恢复性优先于已迁移密钥的残留——保留 .bak，
                // 下一次正常 update 的 Rotate 会用无密钥的 main 覆盖它。
                tracing::warn!(
                    "settings.json.bak still carries the migrated plaintext secret; \
                     kept because it is the only valid settings copy after recovery"
                );
            } else {
                // The backup rotated during this write mirrors the legacy file
                // and may still carry the plaintext secret; drop it.
                let _ = tokio::fs::remove_file(store.backup_path()).await;
            }
        } else if recovered_from_backup
            && let Err(error) = store.persist_with_policy(&value, policy).await
        {
            tracing::warn!(
                %error,
                "failed to rewrite the backup-recovered settings back to settings.json"
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
        // Serialize the whole chain: two concurrent writers used to race past
        // each other and interleave on the shared tmp/backup file names.
        let _write_guard = self.write_lock.lock().await;
        validate_origins(&value.allowed_origins)?;
        value.cloud_sync.normalize();
        self.apply_secret_policy(&mut value).await?;
        self.persist_with_policy(&value, BackupPolicy::RotateValidatedMain)
            .await?;
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

    /// The shared secret is persisted only in the credential store.
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
    ///
    /// Durability contract: under `RotateValidatedMain` the parsed `main` is
    /// copied (never renamed) to `settings.json.bak`, so a complete
    /// settings.json exists on disk at every instant and the `.bak` is the
    /// one-version-older recovery point for the load-side self-heal. Under
    /// `PreserveExisting` (self-heal from `.bak`) the backup is never touched,
    /// because the main on disk is known to be corrupt. In both cases the
    /// unique-named tmp file is fsynced before the final atomic rename, and a
    /// failed rename leaves both main and `.bak` exactly as they were.
    async fn persist_with_policy(&self, value: &AppSettings, policy: BackupPolicy) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        self.cleanup_stale_temporaries().await;
        let mut persisted = value.clone();
        persisted.secret = None;
        let payload = serde_json::to_vec_pretty(&persisted)?;
        let temporary = self
            .path
            .with_extension(format!("json.tmp-{}", uuid::Uuid::new_v4().simple()));
        {
            let mut writer = tokio::io::BufWriter::new(tokio::fs::File::create(&temporary).await?);
            writer.write_all(&payload).await?;
            writer.flush().await?;
            // FlushFileBuffers: a power cut must not leave a zero-byte main
            // behind the final rename.
            writer.get_ref().sync_all().await?;
        }
        // Copy (never rename) the previous main out of the way so the live
        // settings.json is never absent from disk. Only a main this process
        // validated may become the backup: rotating a corrupt main would
        // destroy the single remaining good copy.
        if policy == BackupPolicy::RotateValidatedMain
            && self.path.exists()
            && let Err(error) = tokio::fs::copy(self.path.clone(), self.backup_path()).await
        {
            tracing::warn!(%error, "failed to back up settings.json before overwrite");
        }
        if let Err(error) = tokio::fs::rename(&temporary, &self.path).await {
            // main is untouched; drop the tmp and surface the error.
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error.into());
        }
        Ok(())
    }

    fn backup_path(&self) -> PathBuf {
        self.path.with_extension("json.bak")
    }

    /// Removes `settings.json.tmp-*` leftovers from crashed writers so unique
    /// temporary names cannot accumulate forever. Only files older than
    /// `STALE_TEMPORARY_AGE` are removed: a younger one may be mid-write by a
    /// concurrent process (desktop app vs. MCP child). Anything whose age
    /// cannot be determined (metadata error, clock skew into the future) is
    /// kept.
    async fn cleanup_stale_temporaries(&self) {
        let Some(parent) = self.path.parent() else {
            return;
        };
        let Some(file_name) = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
        else {
            return;
        };
        let prefix = format!("{file_name}.tmp-");
        let Ok(mut entries) = tokio::fs::read_dir(parent).await else {
            return;
        };
        let now = std::time::SystemTime::now();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with(&prefix) {
                continue;
            }
            let Ok(metadata) = entry.metadata().await else {
                continue;
            };
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            if now
                .duration_since(modified)
                .is_ok_and(|age| age >= STALE_TEMPORARY_AGE)
            {
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
    }

    /// Best-effort read of `settings.json.bak`; `None` when it is missing or
    /// unreadable.
    async fn read_backup(path: &Path) -> Option<AppSettings> {
        let backup = path.with_extension("json.bak");
        let bytes = match tokio::fs::read(&backup).await {
            Ok(bytes) => bytes,
            Err(_) => return None,
        };
        match serde_json::from_slice::<AppSettings>(&bytes) {
            Ok(value) => Some(value),
            Err(error) => {
                tracing::warn!(
                    %error,
                    path=%backup.display(),
                    "settings.json.bak is unreadable too"
                );
                None
            }
        }
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

    #[tokio::test]
    async fn recovers_corrupt_settings_from_backup_and_rewrites_it() {
        let (store, root, _credentials) = test_store().await;
        let path = root.join("settings.json");
        // Two updates: the second write's backup captures the first version.
        let mut value = store.current();
        value.language = LanguagePreference::EnUs;
        store.update(value).await.unwrap();
        let mut value = store.current();
        value.language = LanguagePreference::ZhCn;
        store.update(value).await.unwrap();

        let backup_path = root.join("settings.json.bak");
        let backup_before = tokio::fs::read(&backup_path).await.unwrap();
        serde_json::from_slice::<AppSettings>(&backup_before).unwrap();

        tokio::fs::write(&path, b"{ corrupt").await.unwrap();
        let reloaded = SettingsStore::load_with_credential_store(
            path.clone(),
            Arc::new(MemoryCredentialStore::default()),
        )
        .await
        .unwrap();
        assert_eq!(
            reloaded.current().language,
            LanguagePreference::EnUs,
            "the one-version-older backup must win over defaults"
        );

        // The recovery rewrote settings.json so it parses again and no
        // corrupt-*.json archive was needed.
        let healed = tokio::fs::read(&path).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&healed).unwrap();
        assert_eq!(parsed["language"], serde_json::json!("en-US"));
        assert!(!root.read_dir().unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("settings.corrupt-")
        }));

        // Self-heal must never rotate the known-corrupt main into .bak: the
        // only valid copy on disk stays byte-for-byte intact and parseable.
        let backup_after = tokio::fs::read(&backup_path).await.unwrap();
        assert_eq!(
            backup_after, backup_before,
            "settings.json.bak must be preserved untouched during recovery"
        );
        serde_json::from_slice::<AppSettings>(&backup_after).unwrap();
        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn recovers_again_from_the_same_backup_after_a_second_corruption() {
        let (store, root, _credentials) = test_store().await;
        let path = root.join("settings.json");
        let mut value = store.current();
        value.language = LanguagePreference::EnUs;
        store.update(value).await.unwrap();
        let mut value = store.current();
        value.language = LanguagePreference::ZhCn;
        store.update(value).await.unwrap();
        let backup_path = root.join("settings.json.bak");
        let backup_before = tokio::fs::read(&backup_path).await.unwrap();

        // First crash: main corrupt, recovered from .bak and healed.
        tokio::fs::write(&path, b"{ corrupt").await.unwrap();
        let first = SettingsStore::load_with_credential_store(
            path.clone(),
            Arc::new(MemoryCredentialStore::default()),
        )
        .await
        .unwrap();
        assert_eq!(first.current().language, LanguagePreference::EnUs);
        drop(first);

        // Second crash before any normal save: the same backup must still be
        // the recovery point on the next start.
        tokio::fs::write(&path, b"{ corrupt again").await.unwrap();
        let second = SettingsStore::load_with_credential_store(
            path.clone(),
            Arc::new(MemoryCredentialStore::default()),
        )
        .await
        .unwrap();
        assert_eq!(
            second.current().language,
            LanguagePreference::EnUs,
            "a second start must recover from the same untouched backup"
        );
        assert_eq!(tokio::fs::read(&backup_path).await.unwrap(), backup_before);
        assert!(!root.read_dir().unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("settings.corrupt-")
        }));

        // A subsequent normal save rotates the (now valid) main into .bak.
        let mut value = second.current();
        value.language = LanguagePreference::ZhCn;
        second.update(value).await.unwrap();
        let rotated: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&backup_path).await.unwrap()).unwrap();
        assert_eq!(rotated["language"], serde_json::json!("en-US"));
        let _ = tokio::fs::remove_dir_all(root).await;
    }

    /// Recovery write-back failure: main is corrupt and held open by another
    /// process without FILE_SHARE_DELETE, so the tmp -> main rename fails.
    /// Load must still succeed in memory, leave .bak byte-identical, and drop
    /// its temporary file. Windows-only because the share-mode lock has no
    /// portable equivalent.
    #[cfg(windows)]
    #[tokio::test]
    async fn failed_recovery_write_back_leaves_the_backup_intact() {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x0000_0001;

        let (store, root, _credentials) = test_store().await;
        let path = root.join("settings.json");
        let mut value = store.current();
        value.language = LanguagePreference::EnUs;
        store.update(value).await.unwrap();
        let mut value = store.current();
        value.language = LanguagePreference::ZhCn;
        store.update(value).await.unwrap();
        let backup_path = root.join("settings.json.bak");
        let backup_before = tokio::fs::read(&backup_path).await.unwrap();

        tokio::fs::write(&path, b"{ corrupt").await.unwrap();
        // Readers may still open main (so load can detect the corruption), but
        // nobody may delete/replace it while this handle is alive.
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&path)
            .unwrap();

        let recovered = SettingsStore::load_with_credential_store(
            path.clone(),
            Arc::new(MemoryCredentialStore::default()),
        )
        .await
        .expect("a failed write-back must not fail the load itself");
        assert_eq!(recovered.current().language, LanguagePreference::EnUs);
        assert_eq!(
            tokio::fs::read(&path).await.unwrap(),
            b"{ corrupt",
            "the locked main cannot have been replaced"
        );
        let backup_after = tokio::fs::read(&backup_path).await.unwrap();
        assert_eq!(
            backup_after, backup_before,
            "a failed write-back must leave settings.json.bak untouched"
        );
        serde_json::from_slice::<AppSettings>(&backup_after).unwrap();
        assert!(
            !root.read_dir().unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("settings.json.tmp-")
            }),
            "the failed rename must drop its temporary file"
        );
        drop(recovered);
        drop(lock);

        // With the lock gone, the next start heals from that same backup.
        let healed = SettingsStore::load_with_credential_store(
            path.clone(),
            Arc::new(MemoryCredentialStore::default()),
        )
        .await
        .unwrap();
        assert_eq!(healed.current().language, LanguagePreference::EnUs);
        let parsed: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        assert_eq!(parsed["language"], serde_json::json!("en-US"));
        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn cleanup_removes_only_old_settings_temporaries() {
        let (store, root, _credentials) = test_store().await;
        let fresh = root.join("settings.json.tmp-fresh");
        let stale = root.join("settings.json.tmp-stale");
        let future = root.join("settings.json.tmp-future");
        let other = root.join("other.tmp-stale");
        for file in [&fresh, &stale, &future, &other] {
            tokio::fs::write(file, b"{}").await.unwrap();
        }
        let six_minutes_ago = filetime::FileTime::from_system_time(
            std::time::SystemTime::now() - std::time::Duration::from_secs(6 * 60),
        );
        let one_hour_ahead = filetime::FileTime::from_system_time(
            std::time::SystemTime::now() + std::time::Duration::from_secs(60 * 60),
        );
        filetime::set_file_mtime(&stale, six_minutes_ago).unwrap();
        filetime::set_file_mtime(&future, one_hour_ahead).unwrap();
        filetime::set_file_mtime(&other, six_minutes_ago).unwrap();

        let mut value = store.current();
        value.language = LanguagePreference::EnUs;
        store.update(value).await.unwrap();

        assert!(
            fresh.exists(),
            "a temporary younger than the threshold may belong to a concurrent writer"
        );
        assert!(
            !stale.exists(),
            "a stale settings temporary must be removed"
        );
        assert!(
            future.exists(),
            "a temporary with a future mtime (clock skew) must be kept"
        );
        assert!(
            other.exists(),
            "cleanup must only touch settings.json.tmp-* files"
        );
        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn concurrent_updates_serialize_on_the_write_lock() {
        let (store, root, _credentials) = test_store().await;
        let store = Arc::new(store);
        let mut handles = Vec::new();
        for index in 0..10 {
            let store = Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                let mut value = store.current();
                value.allowed_origins = vec![format!("https://example{index}.com")];
                store.update(value).await.unwrap();
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }

        let final_value = store.current();
        assert_eq!(
            final_value.allowed_origins.len(),
            1,
            "exactly one writer's value must survive, not an interleaved merge"
        );
        let raw = tokio::fs::read(root.join("settings.json")).await.unwrap();
        serde_json::from_slice::<AppSettings>(&raw)
            .expect("settings.json must stay parseable under concurrent writers");
        assert_settings_json_has_no_secret(&root).await;
    }

    #[tokio::test]
    async fn persist_keeps_a_backup_and_leaves_no_temporary_files() {
        let (store, root, _credentials) = test_store().await;
        let mut value = store.current();
        value.language = LanguagePreference::EnUs;
        store.update(value).await.unwrap();
        let mut value = store.current();
        value.language = LanguagePreference::ZhCn;
        store.update(value).await.unwrap();

        // main and the kept one-version-older backup both parse.
        let main = tokio::fs::read(root.join("settings.json")).await.unwrap();
        serde_json::from_slice::<AppSettings>(&main).unwrap();
        let backup = tokio::fs::read(root.join("settings.json.bak"))
            .await
            .unwrap();
        serde_json::from_slice::<AppSettings>(&backup).unwrap();
        assert!(
            !root.read_dir().unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("settings.json.tmp-")
            }),
            "persist must clean up its unique temporary files"
        );
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
