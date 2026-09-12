use async_trait::async_trait;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::{Mutex, RwLock};

use crate::{
    database,
    embedding::EmbeddingManager,
    error::{AppError, Result},
    models::{
        ApiStatus, AppSettings, CloudBackendKind, CloudCredentialInput, CloudSyncSettings,
        EmbeddingBackendKind, S3CloudSyncSettings,
    },
    semantic::SemanticEngine,
    settings::SettingsStore,
    sync::{
        backend::{CloudBackend, RemotePath},
        bundle::seal_bundle,
        credentials::{
            CredentialStore, MemoryCredentialStore, SecretKind, SecretValue,
            StoredCloudCredentialProfile, load_credential_bundle, save_credential_bundle,
        },
        engine::{HeadDocument, SyncEngine},
        factory::{backend_from_input, backend_from_store},
        store::SyncStore,
        test_s3_server::TestS3,
        types::{
            BundleChange, BundleContents, EntityKey, EntityVersion, MutationOperation,
            NormalizedSessionSnapshot, SyncTrigger,
        },
        vault::{VaultDocument, VaultIdentity, VaultProtection, load_or_create_vault},
    },
};

use super::super::{AppService, CloudSyncRuntime, CloudSyncScheduler, ServiceRole};

pub(crate) async fn service_with_local_session_fixture() -> (AppService, TempDataDir) {
    let data_dir = TempDataDir::new("service-sync-gate");
    let pool = database::connect(&data_dir.join("chat_memory.db"))
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO sessions (id, platform, platform_session_id, title, raw_data)
         VALUES ('local-session', 'fixture', 'local-1', 'Local session', '{}')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let settings = Arc::new(
        SettingsStore::load(data_dir.join("settings.json"))
            .await
            .unwrap(),
    );
    let mut settings_value = settings.get().await;
    settings_value.semantic_search.backend = EmbeddingBackendKind::Ollama;
    settings.update(settings_value.clone()).await.unwrap();
    let embeddings =
        EmbeddingManager::from_settings(data_dir.path.clone(), settings_value.semantic_search)
            .await
            .unwrap();
    let semantic = Arc::new(SemanticEngine::new(
        pool.clone(),
        data_dir.path.clone(),
        embeddings,
    ));

    (
        AppService {
            pool: pool.clone(),
            settings,
            semantic,
            role: ServiceRole::Desktop,
            data_dir: Arc::new(data_dir.path.clone()),
            api_status: Arc::new(RwLock::new(ApiStatus::Starting)),
            last_userscript_request_at: Arc::new(RwLock::new(None)),
            sync_store: SyncStore::new(pool),
            credentials: Arc::new(MemoryCredentialStore::default()),
            cloud_sync_scheduler: CloudSyncScheduler::for_tests(),
            sync_gate: Arc::new(Mutex::new(())),
            cloud_sync_runtime: Arc::new(RwLock::new(CloudSyncRuntime::default())),
            shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
        data_dir,
    )
}

pub(crate) async fn publish_released_plain_fixture(
    backend: &dyn CloudBackend,
    title: &str,
) -> HeadDocument {
    use sha2::{Digest, Sha256};

    let snapshot = NormalizedSessionSnapshot {
        key: EntityKey {
            platform: "legacy".into(),
            platform_session_id: "remote-only".into(),
        },
        title: title.into(),
        created_at: None,
        updated_at: None,
        imported_at: "2026-08-06T00:00:00Z".into(),
        raw_data: serde_json::json!({"released": true}),
        messages: vec![],
    };
    let content_hash = hex::encode(Sha256::digest(serde_json::to_vec(&snapshot).unwrap()));
    let contents = BundleContents {
        vault_id: "default".into(),
        generation_id: "generation-1".into(),
        device_id: "device-released".into(),
        start_seq: 1,
        end_seq: 1,
        previous_path: None,
        previous_sha256: None,
        previous_end_seq: None,
        changes: vec![BundleChange {
            local_seq: 1,
            key: snapshot.key.clone(),
            operation: MutationOperation::Upsert,
            version: EntityVersion::new(1, 0, "device-released"),
            content_hash: Some(content_hash),
            snapshot: Some(snapshot),
        }],
    };
    let sealed = seal_bundle(&contents).unwrap();
    let path = RemotePath::parse(&format!(
        "v1/generations/generation-1/devices/device-released/bundles/1-1-{}.acmb",
        sealed.file_sha256
    ))
    .unwrap();
    backend.put_immutable(&path, &sealed.bytes).await.unwrap();
    let head = HeadDocument {
        generation_id: "generation-1".into(),
        device_id: "device-released".into(),
        end_seq: 1,
        path: path.display(),
        sha256: sealed.file_sha256,
    };
    backend
        .put_if_absent(
            &RemotePath::parse("v1/generations/generation-1/devices/device-released/head.json")
                .unwrap(),
            &serde_json::to_vec(&head).unwrap(),
        )
        .await
        .unwrap();
    head
}

#[derive(Clone, Default)]
pub(crate) struct FaultInjectingCredentialStore {
    pub(crate) inner: MemoryCredentialStore,
    pub(crate) fail_on_mutation: Arc<AtomicUsize>,
    pub(crate) mutation_count: Arc<AtomicUsize>,
}

impl FaultInjectingCredentialStore {
    pub(crate) fn arm(&self, failure_point: usize) {
        self.mutation_count.store(0, Ordering::SeqCst);
        self.fail_on_mutation.store(failure_point, Ordering::SeqCst);
    }

    fn fail_if_armed(&self) -> Result<()> {
        let mutation = self.mutation_count.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_on_mutation.load(Ordering::SeqCst) == mutation {
            self.fail_on_mutation.store(0, Ordering::SeqCst);
            return Err(AppError::Credential(format!(
                "injected credential mutation failure at {mutation}"
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl CredentialStore for FaultInjectingCredentialStore {
    async fn get(&self, vault_key: &str, kind: SecretKind) -> Result<Option<SecretValue>> {
        self.inner.get(vault_key, kind).await
    }

    async fn set(&self, vault_key: &str, kind: SecretKind, value: SecretValue) -> Result<()> {
        self.fail_if_armed()?;
        self.inner.set(vault_key, kind, value).await
    }

    async fn delete(&self, vault_key: &str, kind: SecretKind) -> Result<()> {
        self.fail_if_armed()?;
        self.inner.delete(vault_key, kind).await
    }
}

pub(crate) async fn configured_encrypted_s3_service(
    credentials: Arc<dyn CredentialStore>,
) -> (
    AppService,
    TestS3,
    AppSettings,
    Arc<dyn CloudBackend>,
    TempDataDir,
) {
    let (mut service, data_dir) = service_with_local_session_fixture().await;
    service.credentials = credentials;
    let server = TestS3::start("AKID", Some("old-token")).await;
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        encryption_enabled: true,
        remote_id: "remote-encryption-rotation".into(),
        vault_id: "vault-encryption-rotation".into(),
        generation_id: "generation-old".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "service-encryption-rotation".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    for (kind, value) in [
        (SecretKind::S3AccessKeyId, "AKID"),
        (SecretKind::S3SecretAccessKey, "secret-key"),
        (SecretKind::S3SessionToken, "old-token"),
        (SecretKind::SyncPassphrase, "old-passphrase"),
    ] {
        service
            .credentials
            .set(
                &settings.cloud_sync.remote_id,
                kind,
                SecretValue::new(value),
            )
            .await
            .unwrap();
    }
    service.ensure_local_device().await.unwrap();
    service.sync_store.seed_local_baseline().await.unwrap();
    let backend = backend_from_store(&settings.cloud_sync, service.credentials.as_ref())
        .await
        .unwrap();
    let protection =
        VaultProtection::encrypted(&settings.cloud_sync.vault_id, "old-passphrase").unwrap();
    load_or_create_vault(
        backend.as_ref(),
        VaultDocument::active(
            VaultIdentity {
                format_version: 2,
                vault_id: settings.cloud_sync.vault_id.clone(),
                generation_id: settings.cloud_sync.generation_id.clone(),
            },
            protection.clone(),
        ),
    )
    .await
    .unwrap();
    let device = service.ensure_local_device().await.unwrap();
    SyncEngine::new_protected_with_policy(
        service.sync_store.clone(),
        backend.clone(),
        &settings.cloud_sync.vault_id,
        &settings.cloud_sync.generation_id,
        device.device_id,
        protection.clone(),
        protection
            .derive_protector(&settings.cloud_sync.vault_id, "old-passphrase")
            .unwrap(),
    )
    .run_once(SyncTrigger::Manual)
    .await
    .unwrap();
    (service, server, settings, backend, data_dir)
}

pub(crate) fn wrong_join_credentials() -> CloudCredentialInput {
    CloudCredentialInput::S3 {
        access_key_id: "AKID".into(),
        secret_access_key: "secret-key".into(),
        session_token: None,
        sync_password: Some("wrong-passphrase".into()),
    }
}

pub(crate) async fn vault_object_snapshot(backend: &dyn CloudBackend) -> (Vec<u8>, Option<String>) {
    let object = backend
        .get(&RemotePath::parse("v1/vault.json").unwrap())
        .await
        .unwrap();
    (object.bytes, object.etag)
}

#[derive(Debug)]
pub(crate) struct TempDataDir {
    pub(crate) path: PathBuf,
}

impl TempDataDir {
    pub(crate) fn new(prefix: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("ai-chat-memory-{prefix}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl std::ops::Deref for TempDataDir {
    type Target = std::path::Path;
    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

impl AsRef<std::path::Path> for TempDataDir {
    fn as_ref(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDataDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub(crate) fn auto_prefix(test_name: &str) -> String {
    format!("{test_name}_{}", uuid::Uuid::new_v4().simple())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeaseLifecycle {
    Expired,
    #[allow(dead_code)]
    Active,
}

impl LeaseLifecycle {
    pub(crate) fn timestamps(self) -> (i64, i64) {
        match self {
            Self::Expired => (1, 2),
            Self::Active => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as i64;
                (now - 1_000, now + 300_000)
            }
        }
    }
}

pub(crate) struct CloudFixture {
    pub(crate) service: AppService,
    pub(crate) server: TestS3,
    pub(crate) backend: Arc<dyn CloudBackend>,
    pub(crate) settings: CloudSyncSettings,
    pub(crate) credentials: Arc<MemoryCredentialStore>,
    pub(crate) data_dir: TempDataDir,
}

impl CloudFixture {
    pub(crate) fn data_dir(&self) -> &std::path::Path {
        &self.data_dir.path
    }

    pub(crate) async fn empty(prefix: &str) -> Self {
        let (mut service, data_dir) = service_with_local_session_fixture().await;
        let credentials = Arc::new(MemoryCredentialStore::default());
        service.credentials = credentials.clone();
        let server = TestS3::start("AKID", None).await;
        let mut settings = service.settings().await;
        settings.cloud_sync = CloudSyncSettings {
            backend: CloudBackendKind::S3,
            enabled: true,
            connection_verified: true,
            encryption_enabled: false,
            remote_id: format!("remote-{prefix}"),
            vault_id: format!("vault-{prefix}"),
            generation_id: "generation-old".into(),
            s3: S3CloudSyncSettings {
                endpoint_url: server.endpoint().into(),
                region: "us-east-1".into(),
                bucket: "archive".into(),
                prefix: prefix.into(),
                force_path_style: true,
            },
            ..CloudSyncSettings::default()
        };
        service.settings.update(settings.clone()).await.unwrap();
        for (kind, value) in [
            (SecretKind::S3AccessKeyId, "AKID"),
            (SecretKind::S3SecretAccessKey, "secret-key"),
        ] {
            credentials
                .set(
                    &settings.cloud_sync.remote_id,
                    kind,
                    SecretValue::new(value),
                )
                .await
                .unwrap();
        }
        let backend = backend_from_store(&settings.cloud_sync, credentials.as_ref())
            .await
            .unwrap();
        Self {
            service,
            server,
            backend,
            settings: settings.cloud_sync,
            credentials,
            data_dir,
        }
    }

    pub(crate) async fn plain(prefix: &str) -> Self {
        let fx = Self::empty(prefix).await;
        load_or_create_vault(
            fx.backend.as_ref(),
            VaultDocument::active(
                VaultIdentity {
                    format_version: 2,
                    vault_id: fx.settings.vault_id.clone(),
                    generation_id: fx.settings.generation_id.clone(),
                },
                VaultProtection::plain(),
            ),
        )
        .await
        .unwrap();
        fx
    }

    pub(crate) async fn encrypted(prefix: &str, passphrase: &str) -> Self {
        let mut fx = Self::empty(prefix)
            .await
            .with_local_passphrase(passphrase)
            .await;
        fx.settings.encryption_enabled = true;
        let mut app_settings = fx.service.settings().await;
        app_settings.cloud_sync.encryption_enabled = true;
        fx.service.settings.update(app_settings).await.unwrap();
        let protection = crate::test_support::test_protection(&fx.settings.vault_id, passphrase);
        load_or_create_vault(
            fx.backend.as_ref(),
            VaultDocument::active(
                VaultIdentity {
                    format_version: 2,
                    vault_id: fx.settings.vault_id.clone(),
                    generation_id: fx.settings.generation_id.clone(),
                },
                protection,
            ),
        )
        .await
        .unwrap();
        fx
    }

    pub(crate) async fn frozen_by_other_device(self, reason: &str, lease: LeaseLifecycle) -> Self {
        let active = crate::sync::vault::load_versioned_identity(self.backend.as_ref())
            .await
            .unwrap();
        let (start, expire) = lease.timestamps();
        crate::sync::vault::begin_generation_freeze_owned(
            self.backend.as_ref(),
            &active.document(),
            "generation-next",
            active.protection.clone(),
            reason,
            "device-other",
            start,
            expire,
        )
        .await
        .unwrap();
        self
    }

    pub(crate) async fn publishing_by_other_device(self, lease: LeaseLifecycle) -> Self {
        let active = crate::sync::vault::load_versioned_identity(self.backend.as_ref())
            .await
            .unwrap();
        let (start, expire) = lease.timestamps();
        let head_path = format!(
            "v1/generations/{}/devices/device-other/head.json",
            self.settings.generation_id
        );
        let head = HeadDocument {
            generation_id: self.settings.generation_id.clone(),
            device_id: "device-other".into(),
            end_seq: 1,
            path: format!(
                "v1/generations/{}/devices/device-other/bundles/1-1-test.acmb",
                self.settings.generation_id
            ),
            sha256: "test".into(),
        };
        crate::sync::vault::begin_head_publish(
            self.backend.as_ref(),
            &active,
            crate::sync::vault::HeadPublishRequest {
                operation_id: "publish-other".into(),
                owner_device_id: "device-other".into(),
                started_at_ms: start,
                lease_expires_at_ms: expire,
                head_path,
                expected_head_etag: None,
                replacement_head_json: serde_json::to_string(&head).unwrap(),
                published_mutation_count: 1,
            },
        )
        .await
        .unwrap();
        self
    }

    pub(crate) async fn with_local_passphrase(self, passphrase: &str) -> Self {
        self.credentials
            .set(
                &self.settings.remote_id,
                SecretKind::SyncPassphrase,
                SecretValue::new(passphrase),
            )
            .await
            .unwrap();
        if let Ok(Some(mut bundle)) =
            load_credential_bundle(self.credentials.as_ref(), &self.settings.remote_id).await
        {
            match &mut bundle.active {
                StoredCloudCredentialProfile::S3 {
                    sync_passphrase, ..
                }
                | StoredCloudCredentialProfile::Webdav {
                    sync_passphrase, ..
                } => {
                    *sync_passphrase = Some(passphrase.to_string());
                }
            }
            save_credential_bundle(self.credentials.as_ref(), &self.settings.remote_id, &bundle)
                .await
                .unwrap();
        }
        self
    }

    pub(crate) async fn with_encryption_enabled(mut self, enabled: bool) -> Self {
        self.settings.encryption_enabled = enabled;
        let mut app_settings = self.service.settings().await;
        app_settings.cloud_sync.encryption_enabled = enabled;
        self.service.settings.update(app_settings).await.unwrap();
        self
    }

    pub(crate) async fn join_candidate(
        prefix: &str,
        passphrase: &str,
    ) -> (Self, CloudSyncSettings) {
        let (service, data_dir) = service_with_local_session_fixture().await;
        let server = TestS3::start("AKID", None).await;
        let draft = CloudSyncSettings {
            backend: CloudBackendKind::S3,
            enabled: true,
            connection_verified: true,
            encryption_enabled: true,
            remote_id: format!("remote-{prefix}"),
            vault_id: format!("candidate-vault-{prefix}"),
            generation_id: "generation-old".into(),
            s3: S3CloudSyncSettings {
                endpoint_url: server.endpoint().into(),
                region: "us-east-1".into(),
                bucket: "archive".into(),
                prefix: prefix.into(),
                force_path_style: true,
            },
            ..CloudSyncSettings::default()
        };
        let creator = CloudCredentialInput::S3 {
            access_key_id: "AKID".into(),
            secret_access_key: "secret-key".into(),
            session_token: None,
            sync_password: Some(passphrase.into()),
        };
        let backend = backend_from_input(&draft, &creator).unwrap();
        load_or_create_vault(
            backend.as_ref(),
            VaultDocument::active(
                VaultIdentity {
                    format_version: 2,
                    vault_id: format!("join-vault-{prefix}"),
                    generation_id: "generation-old".into(),
                },
                crate::test_support::test_protection(&format!("join-vault-{prefix}"), passphrase),
            ),
        )
        .await
        .unwrap();
        let fx = Self {
            service,
            server,
            backend,
            settings: draft.clone(),
            credentials: Arc::new(MemoryCredentialStore::default()),
            data_dir,
        };
        (fx, draft)
    }

    pub(crate) async fn released_v1(prefix: &str, stale_encryption_enabled: bool) -> Self {
        let (service, data_dir) = service_with_local_session_fixture().await;
        let server = TestS3::start("AKID", None).await;
        let mut settings = service.settings().await;
        settings.cloud_sync = CloudSyncSettings {
            backend: CloudBackendKind::S3,
            enabled: true,
            connection_verified: true,
            encryption_enabled: stale_encryption_enabled,
            remote_id: format!("remote-{prefix}"),
            vault_id: "default".into(),
            generation_id: "generation-1".into(),
            s3: S3CloudSyncSettings {
                endpoint_url: server.endpoint().into(),
                region: "us-east-1".into(),
                bucket: "archive".into(),
                prefix: prefix.into(),
                force_path_style: true,
            },
            ..CloudSyncSettings::default()
        };
        service.settings.update(settings.clone()).await.unwrap();
        for (kind, value) in [
            (SecretKind::S3AccessKeyId, "AKID"),
            (SecretKind::S3SecretAccessKey, "secret-key"),
            (SecretKind::SyncPassphrase, "stale-passphrase"),
        ] {
            service
                .credentials
                .set(
                    &settings.cloud_sync.remote_id,
                    kind,
                    SecretValue::new(value),
                )
                .await
                .unwrap();
        }
        let backend = backend_from_store(&settings.cloud_sync, service.credentials.as_ref())
            .await
            .unwrap();
        publish_released_plain_fixture(backend.as_ref(), "released remote only").await;
        Self {
            credentials: Arc::new(MemoryCredentialStore::default()),
            service,
            server,
            backend,
            settings: settings.cloud_sync,
            data_dir,
        }
    }

    pub(crate) async fn assert_cloud_config_unchanged(&self) {
        let current = self.service.settings().await.cloud_sync;
        assert_eq!(
            serde_json::to_value(&current).unwrap(),
            serde_json::to_value(&self.settings).unwrap()
        );
    }

    pub(crate) async fn assert_remote_untouched(&self, snapshot: &(Vec<u8>, Option<String>)) {
        assert_eq!(
            &vault_object_snapshot(self.backend.as_ref()).await,
            snapshot
        );
    }

    pub(crate) async fn assert_generation_layout(&self, expected: &[&str]) {
        crate::test_support::assert_generation_layout(self.backend.as_ref(), expected).await;
    }
}
