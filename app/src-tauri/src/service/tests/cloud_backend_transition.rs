use super::super::{
    AppService, CloudSyncCommand, CloudSyncRuntime, CloudSyncScheduler, CloudSyncWorkerState,
    ServiceRole, VaultPassphrase, VaultVerification, classify_cloud_error, import_local_sessions,
    map_cloud_error, prepare_cloud_sync_transition, validate_cloud_sync_update,
};
use crate::{
    database,
    embedding::EmbeddingManager,
    error::{AppError, Result},
    models::{
        ApiStatus, AppSettings, CloudBackendKind, CloudCredentialInput, CloudSyncSettings,
        CloudSyncState, EmbeddingBackendKind, ImportRequest, NormalizedSession,
        S3CloudSyncSettings,
    },
    semantic::SemanticEngine,
    settings::SettingsStore,
    sync::{
        backend::{CloudBackend, CloudError, CloudErrorKind, RemotePath},
        bundle::seal_bundle,
        credentials::{
            CredentialStore, CredentialTransitionPhase, MemoryCredentialStore,
            PendingCredentialProfile, SecretKind, SecretValue, StoredCloudCredentialProfile,
            StoredCredentialBundle, load_credential_bundle, load_or_migrate_credential_bundle,
            save_credential_bundle,
        },
        engine::{HeadDocument, SyncEngine},
        factory::{backend_from_input, backend_from_store},
        store::SyncStore,
        test_s3_server::TestS3,
        test_server::TestWebDav,
        types::{
            BundleChange, BundleContents, EntityKey, EntityVersion, MutationOperation,
            NormalizedSessionSnapshot, SyncTrigger,
        },
        vault::{
            HeadPublishRequest, VaultDocument, VaultIdentity, VaultProtection, VaultState,
            begin_generation_freeze_owned, begin_head_publish, load_or_create_identity,
            load_or_create_vault, load_versioned_identity, replace_identity,
        },
    },
};
use async_trait::async_trait;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::{Mutex, Notify, RwLock, mpsc};

async fn service_with_local_session_fixture() -> (AppService, PathBuf) {
    let data_dir = std::env::temp_dir().join(format!(
        "ai-chat-memory-service-sync-gate-{}",
        uuid::Uuid::new_v4()
    ));
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
        EmbeddingManager::from_settings(data_dir.clone(), settings_value.semantic_search)
            .await
            .unwrap();
    let semantic = Arc::new(SemanticEngine::new(
        pool.clone(),
        data_dir.clone(),
        embeddings,
    ));

    (
        AppService {
            pool: pool.clone(),
            settings,
            semantic,
            role: ServiceRole::Desktop,
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

async fn service_with_local_session() -> AppService {
    service_with_local_session_fixture().await.0
}

async fn publish_released_plain_fixture(backend: &dyn CloudBackend, title: &str) -> HeadDocument {
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

async fn configured_released_v1_service(
    stale_encryption_enabled: bool,
    prefix: &str,
) -> (AppService, AppSettings, Arc<dyn CloudBackend>, TestS3) {
    let service = service_with_local_session().await;
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
    (service, settings, backend, server)
}

#[derive(Clone, Default)]
struct FaultInjectingCredentialStore {
    inner: MemoryCredentialStore,
    fail_on_mutation: Arc<AtomicUsize>,
    mutation_count: Arc<AtomicUsize>,
}

impl FaultInjectingCredentialStore {
    fn arm(&self, failure_point: usize) {
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

#[tokio::test]
async fn s3_credential_update_rolls_back_an_atomic_bundle_write_failure() {
    const OLD: [&str; 4] = ["OLD-AKID", "old-secret", "old-token", "old-passphrase"];
    for delete_optional in [false, true] {
        let mut service = service_with_local_session().await;
        let new_token = (!delete_optional).then_some("new-token");
        let server = TestS3::start("NEW-AKID", new_token).await;
        let credentials = FaultInjectingCredentialStore::default();
        for (kind, value) in [
            SecretKind::S3AccessKeyId,
            SecretKind::S3SecretAccessKey,
            SecretKind::S3SessionToken,
            SecretKind::SyncPassphrase,
        ]
        .into_iter()
        .zip(OLD)
        {
            credentials
                .inner
                .set("remote-atomic", kind, SecretValue::new(value))
                .await
                .unwrap();
        }
        service.credentials = Arc::new(credentials.clone());
        let mut active = service.settings().await;
        active.cloud_sync = CloudSyncSettings {
            backend: CloudBackendKind::S3,
            connection_verified: true,
            remote_id: "remote-atomic".into(),
            s3: S3CloudSyncSettings {
                endpoint_url: server.endpoint().into(),
                region: "us-east-1".into(),
                bucket: "archive".into(),
                prefix: format!("credential-failure-{delete_optional}"),
                force_path_style: true,
            },
            ..CloudSyncSettings::default()
        };
        service.settings.update(active.clone()).await.unwrap();
        load_or_migrate_credential_bundle(&credentials, &active.cloud_sync)
            .await
            .unwrap()
            .unwrap();
        service.ensure_local_device().await.unwrap();
        service.sync_store.seed_local_baseline().await.unwrap();
        let settings_before = service.settings().await;
        let outbox_before = service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap();
        credentials.arm(1);
        let replacement = CloudCredentialInput::S3 {
            access_key_id: "NEW-AKID".into(),
            secret_access_key: "secret-key".into(),
            session_token: new_token.map(str::to_owned),
            sync_password: (!delete_optional).then(|| "new-passphrase".into()),
        };
        let mut next = settings_before.clone();
        next.setup_complete = !next.setup_complete;

        let error = service
            .update_settings_with_cloud_credentials(next, Some(replacement))
            .await
            .unwrap_err();

        assert!(matches!(error, AppError::Credential(_)));
        assert_eq!(
            serde_json::to_value(service.settings().await).unwrap(),
            serde_json::to_value(settings_before).unwrap(),
            "atomic credential failure changed settings"
        );
        assert_eq!(
            service
                .sync_store
                .pending_mutations(i64::MAX)
                .await
                .unwrap(),
            outbox_before,
            "atomic credential failure changed the outbox"
        );
        let bundle = load_credential_bundle(&credentials, "remote-atomic")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            bundle.active,
            StoredCloudCredentialProfile::S3 {
                access_key_id: OLD[0].into(),
                secret_access_key: OLD[1].into(),
                session_token: Some(OLD[2].into()),
                sync_passphrase: Some(OLD[3].into()),
            }
        );
        assert!(bundle.pending.is_none());
    }
}

#[tokio::test]
async fn settings_write_failure_restores_every_s3_credential_and_the_active_draft() {
    let (mut service, data_dir) = service_with_local_session_fixture().await;
    let server = TestS3::start("NEW-AKID", Some("new-token")).await;
    let credentials = MemoryCredentialStore::default();
    for (kind, value) in [
        (SecretKind::S3AccessKeyId, "OLD-AKID"),
        (SecretKind::S3SecretAccessKey, "old-secret"),
        (SecretKind::S3SessionToken, "old-token"),
        (SecretKind::SyncPassphrase, "old-passphrase"),
    ] {
        credentials
            .set("remote-settings-failure", kind, SecretValue::new(value))
            .await
            .unwrap();
    }
    service.credentials = Arc::new(credentials);
    let mut active = service.settings().await;
    active.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        connection_verified: true,
        remote_id: "remote-settings-failure".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "settings-write-rollback".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(active).await.unwrap();
    service.ensure_local_device().await.unwrap();
    service.sync_store.seed_local_baseline().await.unwrap();
    let settings_before = service.settings().await;
    let outbox_before = service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();
    tokio::fs::create_dir(data_dir.join("settings.json.tmp"))
        .await
        .unwrap();
    let mut next = settings_before.clone();
    next.setup_complete = !next.setup_complete;

    let error = service
        .update_settings_with_cloud_credentials(
            next,
            Some(CloudCredentialInput::S3 {
                access_key_id: "NEW-AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: Some("new-token".into()),
                sync_password: Some("new-passphrase".into()),
            }),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, AppError::Io(_)), "{error:?}");
    assert_eq!(
        serde_json::to_value(service.settings().await).unwrap(),
        serde_json::to_value(settings_before).unwrap()
    );
    assert_eq!(
        service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        outbox_before
    );
    let bundle = load_credential_bundle(service.credentials.as_ref(), "remote-settings-failure")
        .await
        .unwrap()
        .expect("legacy credentials should have been migrated before the failed update");
    assert_eq!(
        bundle.active,
        StoredCloudCredentialProfile::S3 {
            access_key_id: "OLD-AKID".into(),
            secret_access_key: "old-secret".into(),
            session_token: Some("old-token".into()),
            sync_passphrase: Some("old-passphrase".into()),
        }
    );
    assert!(bundle.pending.is_none());
}

#[tokio::test]
async fn backend_switch_defers_generation_replay_without_rewriting_local_versions() {
    let service = service_with_local_session().await;
    service.ensure_local_device().await.unwrap();
    service.sync_store.seed_local_baseline().await.unwrap();
    sqlx::query(
        "UPDATE sync_device_state SET hlc_wall_ms = ?, hlc_counter = ? WHERE singleton = 1",
    )
    .bind(i64::MAX)
    .bind(i64::MAX)
    .execute(&service.pool)
    .await
    .unwrap();
    service
        .credentials
        .set(
            "default",
            SecretKind::WebDavPassword,
            SecretValue::new("old-webdav-password"),
        )
        .await
        .unwrap();
    let settings_before = service.settings().await;
    let outbox_before = service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();
    let server = TestS3::start("AKID", Some("new-token")).await;
    let tested = service
        .test_cloud_sync_connection(
            CloudSyncSettings {
                backend: CloudBackendKind::S3,
                enabled: true,
                s3: S3CloudSyncSettings {
                    endpoint_url: server.endpoint().into(),
                    region: "us-east-1".into(),
                    bucket: "archive".into(),
                    prefix: "baseline-rollback".into(),
                    force_path_style: true,
                },
                ..CloudSyncSettings::default()
            },
            CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: Some("new-token".into()),
                sync_password: None,
            },
        )
        .await
        .unwrap();
    let draft_remote_id = tested.cloud_sync.remote_id.clone();

    let updated = service
        .update_settings_with_cloud_credentials(
            AppSettings {
                cloud_sync: tested.cloud_sync,
                ..settings_before.clone()
            },
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: Some("new-token".into()),
                sync_password: None,
            }),
        )
        .await
        .unwrap();

    assert_eq!(updated.cloud_sync.backend, CloudBackendKind::S3);
    assert_eq!(service.settings().await.cloud_sync, updated.cloud_sync);
    assert_eq!(
        service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        outbox_before
    );
    let previous_bundle = load_credential_bundle(service.credentials.as_ref(), "default")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        previous_bundle.active,
        StoredCloudCredentialProfile::Webdav {
            password: "old-webdav-password".into(),
            sync_passphrase: None,
        }
    );
    let switched_bundle = load_credential_bundle(service.credentials.as_ref(), &draft_remote_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        switched_bundle.active,
        StoredCloudCredentialProfile::S3 {
            access_key_id: "AKID".into(),
            secret_access_key: "secret-key".into(),
            session_token: Some("new-token".into()),
            sync_passphrase: None,
        }
    );
    assert!(switched_bundle.pending.is_none());
}

#[tokio::test]
async fn webdav_to_s3_switch_publishes_live_sessions_and_tombstones_without_touching_webdav() {
    let service = service_with_local_session().await;
    let webdav_server = TestWebDav::start("alice", "dav-password").await;
    let webdav_test = service
        .test_cloud_sync_connection(
            CloudSyncSettings {
                backend: CloudBackendKind::Webdav,
                enabled: true,
                base_url: webdav_server.endpoint().into(),
                root_path: String::new(),
                username: "alice".into(),
                ..CloudSyncSettings::default()
            },
            CloudCredentialInput::Webdav {
                password: "dav-password".into(),
                sync_password: None,
            },
        )
        .await
        .unwrap();
    let webdav_settings = service
        .update_settings_with_cloud_credentials(
            AppSettings {
                cloud_sync: webdav_test.cloud_sync,
                ..service.settings().await
            },
            Some(CloudCredentialInput::Webdav {
                password: "dav-password".into(),
                sync_password: None,
            }),
        )
        .await
        .unwrap();
    service
        .sync_once_locked(webdav_settings.clone())
        .await
        .unwrap();
    let webdav_backend =
        backend_from_store(&webdav_settings.cloud_sync, service.credentials.as_ref())
            .await
            .unwrap();
    let old_vault_before = webdav_backend
        .get(&RemotePath::parse("v1/vault.json").unwrap())
        .await
        .unwrap();

    import_local_sessions(
        &service.pool,
        &[NormalizedSession {
            id: "live-session".into(),
            platform: "fixture".into(),
            platform_session_id: "live-2".into(),
            title: "Live after switch".into(),
            created_at: None,
            updated_at: None,
            imported_at: "2026-08-05T00:00:00Z".into(),
            messages: Vec::new(),
            raw_data: serde_json::json!({"source": "switch-test"}),
        }],
    )
    .await
    .unwrap();
    service.delete("local-session").await.unwrap();
    let webdav_method_count_before_switch = webdav_server.methods().await.len();

    let s3_server = TestS3::start("AKID", None).await;
    let s3_test = service
        .test_cloud_sync_connection(
            CloudSyncSettings {
                backend: CloudBackendKind::S3,
                enabled: true,
                s3: S3CloudSyncSettings {
                    endpoint_url: s3_server.endpoint().into(),
                    region: "us-east-1".into(),
                    bucket: "archive".into(),
                    prefix: "switch-baseline".into(),
                    force_path_style: true,
                },
                ..CloudSyncSettings::default()
            },
            CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: None,
                sync_password: None,
            },
        )
        .await
        .unwrap();
    let s3_settings = service
        .update_settings_with_cloud_credentials(
            AppSettings {
                cloud_sync: s3_test.cloud_sync,
                ..service.settings().await
            },
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: None,
                sync_password: None,
            }),
        )
        .await
        .unwrap();
    service.sync_once_locked(s3_settings.clone()).await.unwrap();

    assert_eq!(
        webdav_server.methods().await.len(),
        webdav_method_count_before_switch
    );
    assert_eq!(
        service.sync_store.pending_mutation_count().await.unwrap(),
        0
    );
    let old_vault_after = webdav_backend
        .get(&RemotePath::parse("v1/vault.json").unwrap())
        .await
        .unwrap();
    assert_eq!(old_vault_after.bytes, old_vault_before.bytes);
    assert_eq!(old_vault_after.etag, old_vault_before.etag);

    let s3_backend = backend_from_store(&s3_settings.cloud_sync, service.credentials.as_ref())
        .await
        .unwrap();
    let device = service.ensure_local_device().await.unwrap();
    let head_path = RemotePath::parse(&format!(
        "v1/generations/{}/devices/{}/head.json",
        s3_settings.cloud_sync.generation_id, device.device_id
    ))
    .unwrap();
    let mut current: Option<HeadDocument> =
        Some(serde_json::from_slice(&s3_backend.get(&head_path).await.unwrap().bytes).unwrap());
    let mut operations = Vec::new();
    while let Some(head) = current {
        let decoded = crate::sync::bundle::open_bundle(
            &s3_backend
                .get(&RemotePath::parse(&head.path).unwrap())
                .await
                .unwrap()
                .bytes,
            &crate::sync::bundle::BundleLimits::default(),
        )
        .unwrap();
        operations.extend(decoded.contents.changes.iter().map(|change| {
            (
                change.key.platform.clone(),
                change.key.platform_session_id.clone(),
                change.operation.clone(),
            )
        }));
        current = match (
            decoded.header.previous_path,
            decoded.header.previous_sha256,
            decoded.header.previous_end_seq,
        ) {
            (Some(path), Some(sha256), Some(end_seq)) => Some(HeadDocument {
                generation_id: s3_settings.cloud_sync.generation_id.clone(),
                device_id: device.device_id.clone(),
                end_seq,
                path,
                sha256,
            }),
            (None, None, None) => None,
            _ => panic!("incomplete S3 baseline chain"),
        };
    }
    assert!(operations.contains(&("fixture".into(), "live-2".into(), MutationOperation::Upsert,)));
    assert!(operations.contains(&(
        "fixture".into(),
        "local-1".into(),
        MutationOperation::Delete,
    )));
}

async fn configured_encrypted_s3_service(
    credentials: Arc<dyn CredentialStore>,
) -> (
    AppService,
    TestS3,
    AppSettings,
    Arc<dyn CloudBackend>,
    PathBuf,
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

#[tokio::test]
async fn sync_password_change_reads_old_chain_and_commits_new_encrypted_generation() {
    let credentials: Arc<dyn CredentialStore> = Arc::new(MemoryCredentialStore::default());
    let (service, _server, settings_before, backend, _data_dir) =
        configured_encrypted_s3_service(credentials).await;
    let old_protection = load_versioned_identity(backend.as_ref())
        .await
        .unwrap()
        .protection;
    let outbox_before = service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();

    let updated = service
        .update_settings_with_cloud_credentials(
            settings_before.clone(),
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: Some("old-token".into()),
                sync_password: Some("new-passphrase".into()),
            }),
        )
        .await
        .unwrap();

    let bundle = crate::sync::credentials::load_credential_bundle(
        service.credentials.as_ref(),
        &updated.cloud_sync.remote_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(bundle.active.sync_passphrase(), Some("new-passphrase"));
    assert!(bundle.pending.is_none());
    service
        .sync_once_locked(updated.clone())
        .await
        .expect("the next process run must use the committed credential profile");

    assert_ne!(
        updated.cloud_sync.generation_id,
        settings_before.cloud_sync.generation_id
    );
    assert_eq!(
        load_versioned_identity(backend.as_ref())
            .await
            .unwrap()
            .identity
            .generation_id,
        updated.cloud_sync.generation_id
    );
    assert_eq!(
        service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        outbox_before
    );
    let head_path = RemotePath::parse(&format!(
        "v1/generations/{}/devices/baseline/head.json",
        updated.cloud_sync.generation_id
    ))
    .unwrap();
    let head: HeadDocument =
        serde_json::from_slice(&backend.get(&head_path).await.unwrap().bytes).unwrap();
    let bundle = backend
        .get(&RemotePath::parse(&head.path).unwrap())
        .await
        .unwrap();
    let new_protector = load_versioned_identity(backend.as_ref())
        .await
        .unwrap()
        .protection
        .derive_protector(&updated.cloud_sync.vault_id, "new-passphrase")
        .unwrap()
        .unwrap();
    crate::sync::bundle::open_bundle_protected(
        &bundle.bytes,
        &crate::sync::bundle::BundleLimits::default(),
        Some(new_protector.as_ref()),
    )
    .unwrap();
    let old_protector = old_protection
        .derive_protector(&updated.cloud_sync.vault_id, "old-passphrase")
        .unwrap()
        .unwrap();
    assert!(
        crate::sync::bundle::open_bundle_protected(
            &bundle.bytes,
            &crate::sync::bundle::BundleLimits::default(),
            Some(old_protector.as_ref()),
        )
        .is_err()
    );
}

#[tokio::test]
async fn restart_reconciles_a_committed_generation_before_selecting_credentials() {
    let memory = MemoryCredentialStore::default();
    let credentials: Arc<dyn CredentialStore> = Arc::new(memory.clone());
    let (service, _server, settings_before, backend, data_dir) =
        configured_encrypted_s3_service(credentials).await;
    let updated = service
        .update_settings_with_cloud_credentials(
            settings_before.clone(),
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: Some("old-token".into()),
                sync_password: Some("new-passphrase".into()),
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        load_versioned_identity(backend.as_ref())
            .await
            .unwrap()
            .identity
            .generation_id,
        updated.cloud_sync.generation_id
    );

    let mut crashed_bundle = StoredCredentialBundle::new(StoredCloudCredentialProfile::S3 {
        access_key_id: "AKID".into(),
        secret_access_key: "secret-key".into(),
        session_token: Some("old-token".into()),
        sync_passphrase: Some("old-passphrase".into()),
    });
    crashed_bundle
        .stage_transition(PendingCredentialProfile {
            credentials: StoredCloudCredentialProfile::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: Some("old-token".into()),
                sync_passphrase: Some("new-passphrase".into()),
            },
            operation_id: "rotation-crash-recovery".into(),
            target_vault_id: updated.cloud_sync.vault_id.clone(),
            target_generation_id: updated.cloud_sync.generation_id.clone(),
            phase: CredentialTransitionPhase::Prepared,
        })
        .unwrap();
    save_credential_bundle(
        &memory,
        &settings_before.cloud_sync.remote_id,
        &crashed_bundle,
    )
    .await
    .unwrap();
    service
        .settings
        .update(settings_before.clone())
        .await
        .unwrap();

    let reloaded_settings = Arc::new(
        SettingsStore::load(data_dir.join("settings.json"))
            .await
            .unwrap(),
    );
    let restarted = AppService {
        pool: service.pool.clone(),
        settings: reloaded_settings,
        semantic: service.semantic.clone(),
        role: ServiceRole::Desktop,
        api_status: Arc::new(RwLock::new(ApiStatus::Starting)),
        last_userscript_request_at: Arc::new(RwLock::new(None)),
        sync_store: SyncStore::new(service.pool.clone()),
        credentials: Arc::new(memory.clone()),
        cloud_sync_scheduler: CloudSyncScheduler::for_tests(),
        sync_gate: Arc::new(Mutex::new(())),
        cloud_sync_runtime: Arc::new(RwLock::new(CloudSyncRuntime::default())),
        shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    restarted
        .sync_once_locked(restarted.settings().await)
        .await
        .expect("restart should converge the pending credential transition");

    let persisted = restarted.settings().await;
    assert_eq!(
        persisted.cloud_sync.generation_id,
        updated.cloud_sync.generation_id
    );
    let reconciled =
        crate::sync::credentials::load_credential_bundle(&memory, &persisted.cloud_sync.remote_id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(reconciled.active.sync_passphrase(), Some("new-passphrase"));
    assert!(reconciled.pending.is_none());
}

#[tokio::test]
async fn restart_rolls_back_an_expired_pending_building_freeze() {
    let (mut service, _data_dir) = service_with_local_session_fixture().await;
    let credentials = MemoryCredentialStore::default();
    service.credentials = Arc::new(credentials.clone());
    let server = TestS3::start("AKID", None).await;
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: "remote-expired-pending-freeze".into(),
        vault_id: "vault-expired-pending-freeze".into(),
        generation_id: "generation-old".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "expired-pending-freeze".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    let active_profile = StoredCloudCredentialProfile::S3 {
        access_key_id: "AKID".into(),
        secret_access_key: "secret-key".into(),
        session_token: None,
        sync_passphrase: None,
    };
    let mut bundle = StoredCredentialBundle::new(active_profile.clone());
    bundle
        .stage_transition(PendingCredentialProfile {
            credentials: active_profile,
            operation_id: "rotation-expired-freeze".into(),
            target_vault_id: settings.cloud_sync.vault_id.clone(),
            target_generation_id: "generation-next".into(),
            phase: CredentialTransitionPhase::Prepared,
        })
        .unwrap();
    save_credential_bundle(&credentials, &settings.cloud_sync.remote_id, &bundle)
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, &credentials)
        .await
        .unwrap();
    let active = VaultDocument::active(
        VaultIdentity {
            format_version: 2,
            vault_id: settings.cloud_sync.vault_id.clone(),
            generation_id: settings.cloud_sync.generation_id.clone(),
        },
        VaultProtection::plain(),
    );
    load_or_create_vault(backend.as_ref(), active.clone())
        .await
        .unwrap();
    begin_generation_freeze_owned(
        backend.as_ref(),
        &active,
        "generation-next",
        VaultProtection::plain(),
        "rotation-expired-freeze",
        "device-restart",
        1,
        2,
    )
    .await
    .unwrap();

    let reconciled = service
        .reconcile_pending_credential_transition(settings.clone())
        .await
        .expect("an expired building freeze should safely roll back");

    assert_eq!(reconciled.cloud_sync.generation_id, "generation-old");
    let remote = load_versioned_identity(backend.as_ref()).await.unwrap();
    assert_eq!(remote.state, VaultState::Active);
    assert_eq!(remote.identity.generation_id, "generation-old");
    let stored = load_credential_bundle(&credentials, &settings.cloud_sync.remote_id)
        .await
        .unwrap()
        .unwrap();
    assert!(stored.pending.is_none());
}

#[tokio::test]
async fn restart_activates_an_expired_pending_ready_freeze() {
    let (mut service, _data_dir) = service_with_local_session_fixture().await;
    let credentials = MemoryCredentialStore::default();
    service.credentials = Arc::new(credentials.clone());
    let server = TestS3::start("AKID", None).await;
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: "remote-expired-ready-freeze".into(),
        vault_id: "vault-expired-ready-freeze".into(),
        generation_id: "generation-old".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "expired-ready-freeze".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    let profile = StoredCloudCredentialProfile::S3 {
        access_key_id: "AKID".into(),
        secret_access_key: "secret-key".into(),
        session_token: None,
        sync_passphrase: None,
    };
    let mut bundle = StoredCredentialBundle::new(profile.clone());
    bundle
        .stage_transition(PendingCredentialProfile {
            credentials: profile,
            operation_id: "rotation-ready-freeze".into(),
            target_vault_id: settings.cloud_sync.vault_id.clone(),
            target_generation_id: "generation-next".into(),
            phase: CredentialTransitionPhase::RemoteFrozen,
        })
        .unwrap();
    save_credential_bundle(&credentials, &settings.cloud_sync.remote_id, &bundle)
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, &credentials)
        .await
        .unwrap();
    let active = VaultDocument::active(
        VaultIdentity {
            format_version: 2,
            vault_id: settings.cloud_sync.vault_id.clone(),
            generation_id: settings.cloud_sync.generation_id.clone(),
        },
        VaultProtection::plain(),
    );
    load_or_create_vault(backend.as_ref(), active.clone())
        .await
        .unwrap();
    let current = load_versioned_identity(backend.as_ref()).await.unwrap();
    let frozen = VaultDocument {
        identity: active.identity,
        protection: active.protection,
        compatibility: None,
        state: VaultState::Frozen {
            operation_id: "rotation-ready-freeze".into(),
            owner_device_id: "device-restart".into(),
            started_at_ms: 1,
            lease_expires_at_ms: 2,
            target_generation_id: "generation-next".into(),
            target_protection: VaultProtection::plain(),
            stage: crate::sync::vault::GenerationMaintenanceStage::ReadyToActivate,
            retire_released_v1_compatibility: false,
        },
    };
    backend
        .put_if_match(
            &RemotePath::parse("v1/vault.json").unwrap(),
            &serde_json::to_vec(&frozen).unwrap(),
            &current.etag,
        )
        .await
        .unwrap();

    let reconciled = service
        .reconcile_pending_credential_transition(settings.clone())
        .await
        .expect("an expired ready freeze should activate the prepared generation");

    assert_eq!(reconciled.cloud_sync.generation_id, "generation-next");
    let remote = load_versioned_identity(backend.as_ref()).await.unwrap();
    assert_eq!(remote.state, VaultState::Active);
    assert_eq!(remote.identity.generation_id, "generation-next");
    let stored = load_credential_bundle(&credentials, &settings.cloud_sync.remote_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.active, bundle.pending.clone().unwrap().credentials);
    assert!(stored.pending.is_none());
}

#[tokio::test]
async fn restart_finishes_a_pending_head_publication_before_reconciling_credentials() {
    let (mut service, _data_dir) = service_with_local_session_fixture().await;
    let credentials = MemoryCredentialStore::default();
    service.credentials = Arc::new(credentials.clone());
    let server = TestS3::start("AKID", None).await;
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: "remote-publishing-restart".into(),
        vault_id: "vault-publishing-restart".into(),
        generation_id: "generation-old".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "publishing-restart".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    let profile = StoredCloudCredentialProfile::S3 {
        access_key_id: "AKID".into(),
        secret_access_key: "secret-key".into(),
        session_token: None,
        sync_passphrase: None,
    };
    let mut bundle = StoredCredentialBundle::new(profile.clone());
    bundle
        .stage_transition(PendingCredentialProfile {
            credentials: profile,
            operation_id: "rotation-after-publish".into(),
            target_vault_id: settings.cloud_sync.vault_id.clone(),
            target_generation_id: "generation-next".into(),
            phase: CredentialTransitionPhase::Prepared,
        })
        .unwrap();
    save_credential_bundle(&credentials, &settings.cloud_sync.remote_id, &bundle)
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, &credentials)
        .await
        .unwrap();
    load_or_create_vault(
        backend.as_ref(),
        VaultDocument::active(
            VaultIdentity {
                format_version: 2,
                vault_id: settings.cloud_sync.vault_id.clone(),
                generation_id: settings.cloud_sync.generation_id.clone(),
            },
            VaultProtection::plain(),
        ),
    )
    .await
    .unwrap();
    let active = load_versioned_identity(backend.as_ref()).await.unwrap();
    let head_path = format!(
        "v1/generations/{}/devices/device-restart/head.json",
        settings.cloud_sync.generation_id
    );
    let head = HeadDocument {
        generation_id: settings.cloud_sync.generation_id.clone(),
        device_id: "device-restart".into(),
        end_seq: 1,
        path: format!(
            "v1/generations/{}/devices/device-restart/bundles/1-1-test.acmb",
            settings.cloud_sync.generation_id
        ),
        sha256: "test".into(),
    };
    begin_head_publish(
        backend.as_ref(),
        &active,
        HeadPublishRequest {
            operation_id: "publish-restart".into(),
            owner_device_id: "device-restart".into(),
            started_at_ms: 1,
            lease_expires_at_ms: 2,
            head_path: head_path.clone(),
            expected_head_etag: None,
            replacement_head_json: serde_json::to_string(&head).unwrap(),
            published_mutation_count: 1,
        },
    )
    .await
    .unwrap();

    let reconciled = service
        .reconcile_pending_credential_transition(settings.clone())
        .await
        .expect("a stored head publication should finish deterministically");

    assert_eq!(reconciled.cloud_sync.generation_id, "generation-old");
    assert_eq!(
        load_versioned_identity(backend.as_ref())
            .await
            .unwrap()
            .state,
        VaultState::Active
    );
    assert_eq!(
        backend
            .get(&RemotePath::parse(&head_path).unwrap())
            .await
            .unwrap()
            .bytes,
        serde_json::to_vec(&head).unwrap()
    );
    let stored = load_credential_bundle(&credentials, &settings.cloud_sync.remote_id)
        .await
        .unwrap()
        .unwrap();
    assert!(stored.pending.is_none());
}

#[tokio::test]
async fn expired_pending_freeze_with_a_wrong_active_passphrase_does_not_touch_remote_state() {
    let (mut service, _data_dir) = service_with_local_session_fixture().await;
    let credentials = MemoryCredentialStore::default();
    service.credentials = Arc::new(credentials.clone());
    let server = TestS3::start("AKID", None).await;
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        encryption_enabled: true,
        remote_id: "remote-fresh-wrong-passphrase".into(),
        vault_id: "vault-fresh-wrong-passphrase".into(),
        generation_id: "generation-old".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "fresh-wrong-passphrase".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    let profile = StoredCloudCredentialProfile::S3 {
        access_key_id: "AKID".into(),
        secret_access_key: "secret-key".into(),
        session_token: None,
        sync_passphrase: Some("wrong-passphrase".into()),
    };
    let mut bundle = StoredCredentialBundle::new(profile.clone());
    bundle
        .stage_transition(PendingCredentialProfile {
            credentials: profile,
            operation_id: "rotation-fresh-freeze".into(),
            target_vault_id: settings.cloud_sync.vault_id.clone(),
            target_generation_id: "generation-next".into(),
            phase: CredentialTransitionPhase::Prepared,
        })
        .unwrap();
    save_credential_bundle(&credentials, &settings.cloud_sync.remote_id, &bundle)
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, &credentials)
        .await
        .unwrap();
    let active = VaultDocument::active(
        VaultIdentity {
            format_version: 2,
            vault_id: settings.cloud_sync.vault_id.clone(),
            generation_id: settings.cloud_sync.generation_id.clone(),
        },
        VaultProtection::encrypted(&settings.cloud_sync.vault_id, "correct-passphrase").unwrap(),
    );
    load_or_create_vault(backend.as_ref(), active.clone())
        .await
        .unwrap();
    begin_generation_freeze_owned(
        backend.as_ref(),
        &active,
        "generation-next",
        VaultProtection::plain(),
        "rotation-fresh-freeze",
        "device-restart",
        1,
        2,
    )
    .await
    .unwrap();
    let vault_path = RemotePath::parse("v1/vault.json").unwrap();
    let before = backend.get(&vault_path).await.unwrap();

    let error = service
        .reconcile_pending_credential_transition(settings)
        .await
        .unwrap_err();

    assert!(matches!(error, AppError::Crypto(_)), "{error:?}");
    let after = backend.get(&vault_path).await.unwrap();
    assert_eq!(after.etag, before.etag);
    assert_eq!(after.bytes, before.bytes);
}

#[tokio::test]
async fn fresh_pending_publishing_with_a_wrong_active_passphrase_does_not_touch_remote_state() {
    let (mut service, _data_dir) = service_with_local_session_fixture().await;
    let credentials = MemoryCredentialStore::default();
    service.credentials = Arc::new(credentials.clone());
    let server = TestS3::start("AKID", None).await;
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        encryption_enabled: true,
        remote_id: "remote-fresh-publishing-wrong-passphrase".into(),
        vault_id: "vault-fresh-publishing-wrong-passphrase".into(),
        generation_id: "generation-old".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "fresh-publishing-wrong-passphrase".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    let profile = StoredCloudCredentialProfile::S3 {
        access_key_id: "AKID".into(),
        secret_access_key: "secret-key".into(),
        session_token: None,
        sync_passphrase: Some("wrong-passphrase".into()),
    };
    let mut bundle = StoredCredentialBundle::new(profile.clone());
    bundle
        .stage_transition(PendingCredentialProfile {
            credentials: profile,
            operation_id: "rotation-fresh-publishing".into(),
            target_vault_id: settings.cloud_sync.vault_id.clone(),
            target_generation_id: "generation-next".into(),
            phase: CredentialTransitionPhase::Prepared,
        })
        .unwrap();
    save_credential_bundle(&credentials, &settings.cloud_sync.remote_id, &bundle)
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, &credentials)
        .await
        .unwrap();
    let active = VaultDocument::active(
        VaultIdentity {
            format_version: 2,
            vault_id: settings.cloud_sync.vault_id.clone(),
            generation_id: settings.cloud_sync.generation_id.clone(),
        },
        VaultProtection::encrypted(&settings.cloud_sync.vault_id, "correct-passphrase").unwrap(),
    );
    load_or_create_vault(backend.as_ref(), active.clone())
        .await
        .unwrap();
    let current = load_versioned_identity(backend.as_ref()).await.unwrap();
    let head_path = format!(
        "v1/generations/{}/devices/device-other/head.json",
        settings.cloud_sync.generation_id
    );
    let replacement_head = HeadDocument {
        generation_id: settings.cloud_sync.generation_id.clone(),
        device_id: "device-other".into(),
        end_seq: 1,
        path: format!(
            "v1/generations/{}/devices/device-other/bundles/1-1-test.acmb",
            settings.cloud_sync.generation_id
        ),
        sha256: "test".into(),
    };
    begin_head_publish(
        backend.as_ref(),
        &current,
        HeadPublishRequest {
            operation_id: "expired-publishing".into(),
            owner_device_id: "device-other".into(),
            started_at_ms: 1,
            lease_expires_at_ms: 2,
            head_path: head_path.clone(),
            expected_head_etag: None,
            replacement_head_json: serde_json::to_string(&replacement_head).unwrap(),
            published_mutation_count: 1,
        },
    )
    .await
    .unwrap();
    let vault_before = vault_object_snapshot(backend.as_ref()).await;
    let head_path = RemotePath::parse(&head_path).unwrap();
    let head_before = backend.get(&head_path).await.ok();

    let error = service
        .reconcile_pending_credential_transition(settings)
        .await
        .unwrap_err();

    assert!(matches!(error, AppError::Crypto(_)), "{error:?}");
    assert_eq!(vault_object_snapshot(backend.as_ref()).await, vault_before);
    assert_eq!(backend.get(&head_path).await.ok(), head_before);
}

async fn configured_s3_service_for_sync_guard_tests(
    prefix: &str,
    encryption_enabled: bool,
    sync_passphrase: &str,
) -> (AppService, AppSettings, Arc<dyn CloudBackend>, TestS3) {
    let (mut service, _data_dir) = service_with_local_session_fixture().await;
    let credentials = MemoryCredentialStore::default();
    service.credentials = Arc::new(credentials.clone());
    let server = TestS3::start("AKID", None).await;
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        encryption_enabled,
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
        (SecretKind::SyncPassphrase, sync_passphrase),
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
    let backend = backend_from_store(&settings.cloud_sync, &credentials)
        .await
        .unwrap();
    (service, settings, backend, server)
}

#[tokio::test]
async fn sync_rejects_remote_plain_when_local_encryption_is_persisted() {
    let (service, settings, backend, _server) =
        configured_s3_service_for_sync_guard_tests("plain-fence", true, "local-passphrase").await;
    load_or_create_vault(
        backend.as_ref(),
        VaultDocument::active(
            VaultIdentity {
                format_version: 2,
                vault_id: settings.cloud_sync.vault_id.clone(),
                generation_id: settings.cloud_sync.generation_id.clone(),
            },
            VaultProtection::plain(),
        ),
    )
    .await
    .unwrap();
    let vault_path = RemotePath::parse("v1/vault.json").unwrap();
    let before = backend.get(&vault_path).await.unwrap();
    let settings_before = service.settings().await;

    let error = service
        .sync_once_locked(settings.clone())
        .await
        .unwrap_err();

    assert!(
        matches!(error, AppError::InvalidData(_) | AppError::Crypto(_)),
        "{error:?}"
    );
    assert_eq!(
        serde_json::to_value(service.settings().await).unwrap(),
        serde_json::to_value(settings_before).unwrap()
    );
    let after = backend.get(&vault_path).await.unwrap();
    assert_eq!(after.etag, before.etag);
    assert_eq!(after.bytes, before.bytes);
    assert!(service.sync_store.device_state().await.unwrap().is_none());
}

#[tokio::test]
async fn sync_wrong_passphrase_does_not_recover_expired_frozen_vault() {
    let (service, mut settings, backend, _server) = configured_s3_service_for_sync_guard_tests(
        "wrong-passphrase-frozen",
        true,
        "wrong-passphrase",
    )
    .await;
    settings.cloud_sync.encryption_enabled = true;
    service.settings.update(settings.clone()).await.unwrap();
    let protection =
        VaultProtection::encrypted(&settings.cloud_sync.vault_id, "correct-passphrase").unwrap();
    let active = VaultDocument::active(
        VaultIdentity {
            format_version: 2,
            vault_id: settings.cloud_sync.vault_id.clone(),
            generation_id: settings.cloud_sync.generation_id.clone(),
        },
        protection,
    );
    load_or_create_vault(backend.as_ref(), active.clone())
        .await
        .unwrap();
    begin_generation_freeze_owned(
        backend.as_ref(),
        &active,
        "generation-next",
        VaultProtection::plain(),
        "expired-freeze",
        "device-other",
        1,
        2,
    )
    .await
    .unwrap();
    let vault_path = RemotePath::parse("v1/vault.json").unwrap();
    let before = backend.get(&vault_path).await.unwrap();
    let settings_before = service.settings().await;

    let error = service
        .sync_once_locked(settings.clone())
        .await
        .unwrap_err();

    assert!(matches!(error, AppError::Crypto(_)), "{error:?}");
    assert_eq!(
        serde_json::to_value(service.settings().await).unwrap(),
        serde_json::to_value(settings_before).unwrap()
    );
    let after = backend.get(&vault_path).await.unwrap();
    assert_eq!(after.etag, before.etag);
    assert_eq!(after.bytes, before.bytes);
}

#[tokio::test]
async fn sync_wrong_passphrase_does_not_recover_publishing_vault() {
    let (service, mut settings, backend, _server) = configured_s3_service_for_sync_guard_tests(
        "wrong-passphrase-publishing",
        true,
        "wrong-passphrase",
    )
    .await;
    settings.cloud_sync.encryption_enabled = true;
    service.settings.update(settings.clone()).await.unwrap();
    let protection =
        VaultProtection::encrypted(&settings.cloud_sync.vault_id, "correct-passphrase").unwrap();
    let active = VaultDocument::active(
        VaultIdentity {
            format_version: 2,
            vault_id: settings.cloud_sync.vault_id.clone(),
            generation_id: settings.cloud_sync.generation_id.clone(),
        },
        protection,
    );
    load_or_create_vault(backend.as_ref(), active)
        .await
        .unwrap();
    let current = load_versioned_identity(backend.as_ref()).await.unwrap();
    let head_path = format!(
        "v1/generations/{}/devices/device-other/head.json",
        settings.cloud_sync.generation_id
    );
    let replacement_head = HeadDocument {
        generation_id: settings.cloud_sync.generation_id.clone(),
        device_id: "device-other".into(),
        end_seq: 1,
        path: format!(
            "v1/generations/{}/devices/device-other/bundles/1-1-test.acmb",
            settings.cloud_sync.generation_id
        ),
        sha256: "test".into(),
    };
    begin_head_publish(
        backend.as_ref(),
        &current,
        HeadPublishRequest {
            operation_id: "expired-publish".into(),
            owner_device_id: "device-other".into(),
            started_at_ms: 1,
            lease_expires_at_ms: 2,
            head_path: head_path.clone(),
            expected_head_etag: None,
            replacement_head_json: serde_json::to_string(&replacement_head).unwrap(),
            published_mutation_count: 1,
        },
    )
    .await
    .unwrap();
    let vault_path = RemotePath::parse("v1/vault.json").unwrap();
    let before = backend.get(&vault_path).await.unwrap();
    let head_path = RemotePath::parse(&head_path).unwrap();
    let before_head = backend.get(&head_path).await.ok();
    let settings_before = service.settings().await;

    let error = service
        .sync_once_locked(settings.clone())
        .await
        .unwrap_err();

    assert!(matches!(error, AppError::Crypto(_)), "{error:?}");
    assert_eq!(
        serde_json::to_value(service.settings().await).unwrap(),
        serde_json::to_value(settings_before).unwrap()
    );
    let after = backend.get(&vault_path).await.unwrap();
    assert_eq!(after.etag, before.etag);
    assert_eq!(after.bytes, before.bytes);
    assert_eq!(backend.get(&head_path).await.ok(), before_head);
}

#[tokio::test]
async fn sync_rejects_plain_active_from_a_frozen_recovery_reread_without_mutation() {
    let (service, settings, backend, server) = configured_s3_service_for_sync_guard_tests(
        "frozen-reread-plain",
        true,
        "correct-passphrase",
    )
    .await;
    let active = VaultDocument::active(
        VaultIdentity {
            format_version: 2,
            vault_id: settings.cloud_sync.vault_id.clone(),
            generation_id: settings.cloud_sync.generation_id.clone(),
        },
        VaultProtection::encrypted(&settings.cloud_sync.vault_id, "correct-passphrase").unwrap(),
    );
    load_or_create_vault(backend.as_ref(), active.clone())
        .await
        .unwrap();
    begin_generation_freeze_owned(
        backend.as_ref(),
        &active,
        "generation-next",
        VaultProtection::plain(),
        "expired-frozen-reread-plain",
        "device-other",
        1,
        2,
    )
    .await
    .unwrap();
    let device = service.ensure_local_device().await.unwrap();
    service.sync_store.seed_local_baseline().await.unwrap();
    let head_path = RemotePath::parse(&format!(
        "v1/generations/{}/devices/{}/head.json",
        settings.cloud_sync.generation_id, device.device_id
    ))
    .unwrap();
    let settings_before = service.settings().await;
    let vault_before = vault_object_snapshot(backend.as_ref()).await;
    let head_before = backend.get(&head_path).await.ok();
    let outbox_before = service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();
    let device_before = service.sync_store.device_state().await.unwrap();
    let scripted = VaultDocument::active(active.identity.clone(), VaultProtection::plain());
    server
        .script_vault_change_after_gets(1, serde_json::to_vec(&scripted).unwrap())
        .await;

    let error = service.sync_now().await.unwrap_err();

    assert!(matches!(error, AppError::InvalidData(_)), "{error:?}");
    assert_eq!(
        serde_json::to_value(service.settings().await).unwrap(),
        serde_json::to_value(settings_before).unwrap()
    );
    assert_eq!(vault_object_snapshot(backend.as_ref()).await, vault_before);
    assert_eq!(backend.get(&head_path).await.ok(), head_before);
    assert_eq!(
        service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        outbox_before
    );
    assert_eq!(
        service.sync_store.device_state().await.unwrap(),
        device_before
    );
}

#[tokio::test]
async fn verified_vault_rejects_changed_identity_from_a_frozen_recovery_reread() {
    let (service, settings, backend, server) = configured_s3_service_for_sync_guard_tests(
        "frozen-reread-identity",
        true,
        "correct-passphrase",
    )
    .await;
    let active = VaultDocument::active(
        VaultIdentity {
            format_version: 2,
            vault_id: settings.cloud_sync.vault_id.clone(),
            generation_id: settings.cloud_sync.generation_id.clone(),
        },
        VaultProtection::encrypted(&settings.cloud_sync.vault_id, "correct-passphrase").unwrap(),
    );
    load_or_create_vault(backend.as_ref(), active.clone())
        .await
        .unwrap();
    begin_generation_freeze_owned(
        backend.as_ref(),
        &active,
        "generation-next",
        VaultProtection::plain(),
        "expired-frozen-reread-identity",
        "device-other",
        1,
        2,
    )
    .await
    .unwrap();
    let vault_before = vault_object_snapshot(backend.as_ref()).await;
    let changed_vault_id = "vault-changed-during-recovery";
    let scripted = VaultDocument::active(
        VaultIdentity {
            format_version: 2,
            vault_id: changed_vault_id.into(),
            generation_id: settings.cloud_sync.generation_id.clone(),
        },
        VaultProtection::encrypted(changed_vault_id, "correct-passphrase").unwrap(),
    );
    server
        .script_vault_change_after_gets(1, serde_json::to_vec(&scripted).unwrap())
        .await;

    let result = service
        .load_verified_vault(
            backend.as_ref(),
            &settings.cloud_sync,
            VaultVerification {
                create_if_missing: false,
                expected_vault_id: Some(&settings.cloud_sync.vault_id),
                fence_encryption_enabled: true,
                expected_algorithm: None,
                proposed: None,
                passphrase: VaultPassphrase::Stored,
            },
        )
        .await;
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("changed vault identity was accepted after frozen recovery"),
    };

    assert!(matches!(error, AppError::InvalidData(_)), "{error:?}");
    assert_eq!(vault_object_snapshot(backend.as_ref()).await, vault_before);
}

async fn configured_join_s3_service_for_guard_tests(
    prefix: &str,
) -> (AppService, CloudSyncSettings, Arc<dyn CloudBackend>, TestS3) {
    let service = service_with_local_session().await;
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
        sync_password: Some("correct-passphrase".into()),
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
            VaultProtection::encrypted(&format!("join-vault-{prefix}"), "correct-passphrase")
                .unwrap(),
        ),
    )
    .await
    .unwrap();
    (service, draft, backend, server)
}

fn wrong_join_credentials() -> CloudCredentialInput {
    CloudCredentialInput::S3 {
        access_key_id: "AKID".into(),
        secret_access_key: "secret-key".into(),
        session_token: None,
        sync_password: Some("wrong-passphrase".into()),
    }
}

async fn vault_object_snapshot(backend: &dyn CloudBackend) -> (Vec<u8>, Option<String>) {
    let object = backend
        .get(&RemotePath::parse("v1/vault.json").unwrap())
        .await
        .unwrap();
    (object.bytes, object.etag)
}

#[tokio::test]
async fn saving_settings_rejects_remote_plain_when_local_encryption_is_persisted() {
    let (service, settings_before, backend, _server) =
        configured_s3_service_for_sync_guard_tests("settings-plain-fence", true, "old-passphrase")
            .await;
    load_or_create_vault(
        backend.as_ref(),
        VaultDocument::active(
            VaultIdentity {
                format_version: 2,
                vault_id: settings_before.cloud_sync.vault_id.clone(),
                generation_id: settings_before.cloud_sync.generation_id.clone(),
            },
            VaultProtection::plain(),
        ),
    )
    .await
    .unwrap();
    let vault_before = vault_object_snapshot(backend.as_ref()).await;
    let device = service.ensure_local_device().await.unwrap();
    let head_path = RemotePath::parse(&format!(
        "v1/generations/{}/devices/{}/head.json",
        settings_before.cloud_sync.generation_id, device.device_id
    ))
    .unwrap();
    let head_before = backend.get(&head_path).await.ok();
    let outbox_before = service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();
    let mut next = settings_before.clone();
    next.setup_complete = !next.setup_complete;

    let error = service
        .update_settings_with_cloud_credentials(
            next,
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: None,
                sync_password: Some("old-passphrase".into()),
            }),
        )
        .await
        .unwrap_err();

    assert!(
        matches!(error, AppError::InvalidData(_) | AppError::Crypto(_)),
        "{error:?}"
    );
    assert_eq!(
        serde_json::to_value(service.settings().await).unwrap(),
        serde_json::to_value(settings_before).unwrap()
    );
    assert_eq!(vault_object_snapshot(backend.as_ref()).await, vault_before);
    assert_eq!(backend.get(&head_path).await.ok(), head_before);
    assert_eq!(
        service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        outbox_before
    );
}

#[tokio::test]
async fn joining_existing_vault_with_wrong_passphrase_does_not_recover_expired_frozen_vault() {
    let (service, draft, backend, _server) =
        configured_join_s3_service_for_guard_tests("join-wrong-frozen").await;
    let current = load_versioned_identity(backend.as_ref()).await.unwrap();
    let active = current.document();
    begin_generation_freeze_owned(
        backend.as_ref(),
        &active,
        "generation-next",
        VaultProtection::plain(),
        "join-expired-freeze",
        "device-other",
        1,
        2,
    )
    .await
    .unwrap();
    let vault_before = vault_object_snapshot(backend.as_ref()).await;
    let settings_before = service.settings().await;
    let outbox_before = service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();

    let error = service
        .update_settings_with_cloud_credentials(
            AppSettings {
                cloud_sync: draft.clone(),
                ..settings_before.clone()
            },
            Some(wrong_join_credentials()),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, AppError::Crypto(_)), "{error:?}");
    assert_eq!(
        serde_json::to_value(service.settings().await).unwrap(),
        serde_json::to_value(settings_before).unwrap()
    );
    assert_eq!(vault_object_snapshot(backend.as_ref()).await, vault_before);
    assert_eq!(
        service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        outbox_before
    );
}

#[tokio::test]
async fn joining_existing_vault_with_wrong_passphrase_does_not_recover_publishing_vault() {
    let (service, draft, backend, _server) =
        configured_join_s3_service_for_guard_tests("join-wrong-publishing").await;
    let current = load_versioned_identity(backend.as_ref()).await.unwrap();
    let head_path = format!(
        "v1/generations/{}/devices/device-other/head.json",
        current.identity.generation_id
    );
    let replacement_head = HeadDocument {
        generation_id: current.identity.generation_id.clone(),
        device_id: "device-other".into(),
        end_seq: 1,
        path: format!(
            "v1/generations/{}/devices/device-other/bundles/1-1-test.acmb",
            current.identity.generation_id
        ),
        sha256: "test".into(),
    };
    begin_head_publish(
        backend.as_ref(),
        &current,
        HeadPublishRequest {
            operation_id: "join-expired-publish".into(),
            owner_device_id: "device-other".into(),
            started_at_ms: 1,
            lease_expires_at_ms: 2,
            head_path: head_path.clone(),
            expected_head_etag: None,
            replacement_head_json: serde_json::to_string(&replacement_head).unwrap(),
            published_mutation_count: 1,
        },
    )
    .await
    .unwrap();
    let vault_before = vault_object_snapshot(backend.as_ref()).await;
    let head_path = RemotePath::parse(&head_path).unwrap();
    let head_before = backend.get(&head_path).await.ok();
    let settings_before = service.settings().await;
    let outbox_before = service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();

    let error = service
        .update_settings_with_cloud_credentials(
            AppSettings {
                cloud_sync: draft.clone(),
                ..settings_before.clone()
            },
            Some(wrong_join_credentials()),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, AppError::Crypto(_)), "{error:?}");
    assert_eq!(
        serde_json::to_value(service.settings().await).unwrap(),
        serde_json::to_value(settings_before).unwrap()
    );
    assert_eq!(vault_object_snapshot(backend.as_ref()).await, vault_before);
    assert_eq!(backend.get(&head_path).await.ok(), head_before);
    assert_eq!(
        service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        outbox_before
    );
}

#[tokio::test]
async fn rewrite_rejects_remote_plain_when_local_encryption_is_persisted() {
    let (service, settings_before, backend, _server) =
        configured_s3_service_for_sync_guard_tests("rewrite-plain-fence", true, "old-passphrase")
            .await;
    load_or_create_vault(
        backend.as_ref(),
        VaultDocument::active(
            VaultIdentity {
                format_version: 2,
                vault_id: settings_before.cloud_sync.vault_id.clone(),
                generation_id: settings_before.cloud_sync.generation_id.clone(),
            },
            VaultProtection::plain(),
        ),
    )
    .await
    .unwrap();
    let vault_before = vault_object_snapshot(backend.as_ref()).await;
    let device = service.ensure_local_device().await.unwrap();
    let head_path = RemotePath::parse(&format!(
        "v1/generations/{}/devices/{}/head.json",
        settings_before.cloud_sync.generation_id, device.device_id
    ))
    .unwrap();
    let head_before = backend.get(&head_path).await.ok();
    let outbox_before = service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();

    let error = service.rewrite_cloud_archive().await.unwrap_err();

    assert!(matches!(error, AppError::InvalidData(_)), "{error:?}");
    assert_eq!(
        serde_json::to_value(service.settings().await).unwrap(),
        serde_json::to_value(settings_before).unwrap()
    );
    assert_eq!(vault_object_snapshot(backend.as_ref()).await, vault_before);
    assert_eq!(backend.get(&head_path).await.ok(), head_before);
    assert_eq!(
        service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        outbox_before
    );
}

#[tokio::test]
async fn enabling_encryption_reads_the_old_plain_chain_and_commits_an_encrypted_generation() {
    let (service, settings_before, backend, _server) =
        configured_s3_service_for_sync_guard_tests("enable-encryption", false, "old-passphrase")
            .await;
    load_or_create_vault(
        backend.as_ref(),
        VaultDocument::active(
            VaultIdentity {
                format_version: 2,
                vault_id: settings_before.cloud_sync.vault_id.clone(),
                generation_id: settings_before.cloud_sync.generation_id.clone(),
            },
            VaultProtection::plain(),
        ),
    )
    .await
    .unwrap();

    let mut next = settings_before.clone();
    next.cloud_sync.encryption_enabled = true;
    let updated = service
        .update_settings_with_cloud_credentials(
            next,
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: None,
                sync_password: Some("new-passphrase".into()),
            }),
        )
        .await
        .unwrap();

    assert!(updated.cloud_sync.encryption_enabled);
    assert_ne!(
        updated.cloud_sync.generation_id,
        settings_before.cloud_sync.generation_id
    );
    let remote = load_versioned_identity(backend.as_ref()).await.unwrap();
    assert_eq!(
        remote.identity.generation_id,
        updated.cloud_sync.generation_id
    );
    assert_eq!(
        remote.protection.algorithm,
        crate::sync::bundle::ProtectionAlgorithm::XChaCha20Poly1305
    );
}

#[tokio::test]
async fn disabling_encryption_reads_the_old_chain_and_commits_a_plain_generation() {
    let credentials: Arc<dyn CredentialStore> = Arc::new(MemoryCredentialStore::default());
    let (service, _server, settings_before, backend, _data_dir) =
        configured_encrypted_s3_service(credentials).await;
    let mut next = settings_before.clone();
    next.cloud_sync.encryption_enabled = false;

    let updated = service
        .update_settings_with_cloud_credentials(
            next,
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: Some("old-token".into()),
                sync_password: None,
            }),
        )
        .await
        .unwrap();

    assert!(!updated.cloud_sync.encryption_enabled);
    assert_ne!(
        updated.cloud_sync.generation_id,
        settings_before.cloud_sync.generation_id
    );
    let stored =
        load_credential_bundle(service.credentials.as_ref(), &updated.cloud_sync.remote_id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(stored.active.sync_passphrase(), None);
    assert!(stored.pending.is_none());
    assert_eq!(
        load_versioned_identity(backend.as_ref())
            .await
            .unwrap()
            .identity
            .generation_id,
        updated.cloud_sync.generation_id
    );
    let head_path = RemotePath::parse(&format!(
        "v1/generations/{}/devices/baseline/head.json",
        updated.cloud_sync.generation_id
    ))
    .unwrap();
    let head: HeadDocument =
        serde_json::from_slice(&backend.get(&head_path).await.unwrap().bytes).unwrap();
    let bundle = backend
        .get(&RemotePath::parse(&head.path).unwrap())
        .await
        .unwrap();
    let decoded = crate::sync::bundle::open_bundle(
        &bundle.bytes,
        &crate::sync::bundle::BundleLimits::default(),
    )
    .unwrap();
    assert_eq!(
        decoded.header.protection,
        crate::sync::bundle::ProtectionAlgorithm::Plain
    );
}

#[tokio::test]
async fn credential_failure_before_rotation_leaves_the_remote_generation_untouched() {
    let injecting = FaultInjectingCredentialStore::default();
    let credentials: Arc<dyn CredentialStore> = Arc::new(injecting.clone());
    let (service, _server, settings_before, backend, _data_dir) =
        configured_encrypted_s3_service(credentials).await;
    let outbox_before = service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();
    injecting.arm(1);

    let error = service
        .update_settings_with_cloud_credentials(
            settings_before.clone(),
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: Some("old-token".into()),
                sync_password: Some("new-passphrase".into()),
            }),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, AppError::Credential(_)), "{error:?}");
    assert_eq!(
        serde_json::to_value(service.settings().await).unwrap(),
        serde_json::to_value(settings_before.clone()).unwrap()
    );
    assert_eq!(
        service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        outbox_before
    );
    let bundle = load_credential_bundle(
        service.credentials.as_ref(),
        &settings_before.cloud_sync.remote_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(bundle.active.sync_passphrase(), Some("old-passphrase"));
    assert!(bundle.pending.is_none());
    assert_eq!(
        load_versioned_identity(backend.as_ref())
            .await
            .unwrap()
            .identity
            .generation_id,
        settings_before.cloud_sync.generation_id
    );
    let generations = backend
        .list_depth_one(&RemotePath::parse("v1/generations").unwrap())
        .await
        .unwrap();
    assert_eq!(
        generations
            .into_iter()
            .filter(|entry| entry.is_collection)
            .map(|entry| entry.name)
            .collect::<Vec<_>>(),
        vec![settings_before.cloud_sync.generation_id]
    );
}

#[tokio::test]
async fn settings_write_failure_after_rotation_keeps_the_active_generation_and_new_credentials() {
    let credentials: Arc<dyn CredentialStore> = Arc::new(MemoryCredentialStore::default());
    let (service, _server, settings_before, backend, data_dir) =
        configured_encrypted_s3_service(credentials).await;
    tokio::fs::create_dir(data_dir.join("settings.json.tmp"))
        .await
        .unwrap();

    let error = service
        .update_settings_with_cloud_credentials(
            settings_before.clone(),
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: Some("old-token".into()),
                sync_password: Some("new-passphrase".into()),
            }),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, AppError::Io(_)), "{error:?}");
    assert_eq!(
        service.settings().await.cloud_sync.generation_id,
        settings_before.cloud_sync.generation_id
    );
    let bundle = load_credential_bundle(
        service.credentials.as_ref(),
        &settings_before.cloud_sync.remote_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(bundle.active.sync_passphrase(), Some("old-passphrase"));
    let pending = bundle
        .pending
        .as_ref()
        .expect("a committed remote rotation must remain recoverable");
    assert_eq!(
        pending.credentials.sync_passphrase(),
        Some("new-passphrase")
    );
    assert_eq!(pending.phase, CredentialTransitionPhase::RemoteCommitted);
    let remote = load_versioned_identity(backend.as_ref()).await.unwrap();
    assert_ne!(
        remote.identity.generation_id,
        settings_before.cloud_sync.generation_id
    );
    assert!(
        remote
            .protection
            .passphrase_matches(&remote.identity.vault_id, "new-passphrase")
            .unwrap()
    );
    assert!(
        !remote
            .protection
            .passphrase_matches(&remote.identity.vault_id, "old-passphrase")
            .unwrap()
    );
    let generations = backend
        .list_depth_one(&RemotePath::parse("v1/generations").unwrap())
        .await
        .unwrap()
        .into_iter()
        .filter(|entry| entry.is_collection)
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    assert!(generations.contains(&settings_before.cloud_sync.generation_id));
    assert!(generations.contains(&remote.identity.generation_id));
}

#[tokio::test]
async fn matching_remote_passphrase_corrects_the_credential_without_rotating_generation() {
    let credentials = MemoryCredentialStore::default();
    let credentials_arc: Arc<dyn CredentialStore> = Arc::new(credentials.clone());
    let (service, _server, settings_before, backend, _data_dir) =
        configured_encrypted_s3_service(credentials_arc).await;
    credentials
        .set(
            &settings_before.cloud_sync.remote_id,
            SecretKind::SyncPassphrase,
            SecretValue::new("incorrect-local-copy"),
        )
        .await
        .unwrap();

    let updated = service
        .update_settings_with_cloud_credentials(
            settings_before.clone(),
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: Some("old-token".into()),
                sync_password: Some("old-passphrase".into()),
            }),
        )
        .await
        .unwrap();

    assert_eq!(
        updated.cloud_sync.generation_id,
        settings_before.cloud_sync.generation_id
    );
    assert_eq!(
        load_versioned_identity(backend.as_ref())
            .await
            .unwrap()
            .identity
            .generation_id,
        settings_before.cloud_sync.generation_id
    );
    let bundle = load_credential_bundle(&credentials, &settings_before.cloud_sync.remote_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bundle.active.sync_passphrase(), Some("old-passphrase"));
    assert!(bundle.pending.is_none());
    let generations = backend
        .list_depth_one(&RemotePath::parse("v1/generations").unwrap())
        .await
        .unwrap()
        .into_iter()
        .filter(|entry| entry.is_collection)
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    assert_eq!(generations, vec![settings_before.cloud_sync.generation_id]);
}

#[tokio::test]
async fn connection_test_prepares_a_draft_without_persisting_local_or_remote_state() {
    let service = service_with_local_session().await;
    service.ensure_local_device().await.unwrap();
    service.sync_store.seed_local_baseline().await.unwrap();
    service
        .credentials
        .set(
            "default",
            SecretKind::WebDavPassword,
            SecretValue::new("old-webdav-password"),
        )
        .await
        .unwrap();
    let settings_before = service.settings().await;
    let outbox_before = service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();
    let server = TestS3::start("AKID", Some("session-token")).await;
    let draft = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: false,
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "draft-only".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    let credentials = CloudCredentialInput::S3 {
        access_key_id: "AKID".into(),
        secret_access_key: "secret-key".into(),
        session_token: Some("session-token".into()),
        sync_password: None,
    };

    let result = service
        .test_cloud_sync_connection(draft.clone(), credentials.clone())
        .await
        .unwrap();

    assert!(result.ok);
    assert!(result.cloud_sync.connection_verified);
    assert_eq!(result.cloud_sync.backend, CloudBackendKind::S3);
    assert_eq!(
        serde_json::to_value(service.settings().await).unwrap(),
        serde_json::to_value(settings_before).unwrap(),
        "connection testing changed active settings"
    );
    assert_eq!(
        service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        outbox_before,
        "connection testing changed the live outbox"
    );
    assert_eq!(
        service
            .credentials
            .get("default", SecretKind::WebDavPassword)
            .await
            .unwrap()
            .unwrap()
            .expose_secret(),
        "old-webdav-password"
    );
    assert!(
        service
            .credentials
            .get(&result.cloud_sync.remote_id, SecretKind::S3AccessKeyId)
            .await
            .unwrap()
            .is_none(),
        "connection testing persisted draft credentials"
    );
    let backend = backend_from_input(&result.cloud_sync, &credentials).unwrap();
    assert_eq!(
        backend
            .get(&RemotePath::parse("v1/vault.json").unwrap())
            .await
            .unwrap_err()
            .kind(),
        "not_found",
        "connection testing created the active vault identity"
    );
}

#[tokio::test]
async fn joining_an_existing_vault_rejects_plain_and_encrypted_policy_mismatches() {
    for remote_encrypted in [false, true] {
        let service = service_with_local_session().await;
        let server = TestS3::start("AKID", None).await;
        let requested_encryption = !remote_encrypted;
        let cloud_sync = CloudSyncSettings {
            backend: CloudBackendKind::S3,
            enabled: true,
            encryption_enabled: requested_encryption,
            s3: S3CloudSyncSettings {
                endpoint_url: server.endpoint().into(),
                region: "us-east-1".into(),
                bucket: "archive".into(),
                prefix: format!("join-policy-mismatch-{remote_encrypted}"),
                force_path_style: true,
            },
            ..CloudSyncSettings::default()
        };
        let credentials = CloudCredentialInput::S3 {
            access_key_id: "AKID".into(),
            secret_access_key: "secret-key".into(),
            session_token: None,
            sync_password: requested_encryption.then(|| "candidate-passphrase".into()),
        };
        let backend = backend_from_input(&cloud_sync, &credentials).unwrap();
        let remote_protection = if remote_encrypted {
            VaultProtection::encrypted("remote-vault", "remote-passphrase").unwrap()
        } else {
            VaultProtection::plain()
        };
        load_or_create_vault(
            backend.as_ref(),
            VaultDocument::active(
                VaultIdentity {
                    format_version: 2,
                    vault_id: "remote-vault".into(),
                    generation_id: "remote-generation".into(),
                },
                remote_protection.clone(),
            ),
        )
        .await
        .unwrap();
        let tested = service
            .test_cloud_sync_connection(cloud_sync, credentials.clone())
            .await
            .unwrap();
        let remote_id = tested.cloud_sync.remote_id.clone();
        let settings_before = service.settings().await;

        let error = service
            .update_settings_with_cloud_credentials(
                AppSettings {
                    cloud_sync: tested.cloud_sync,
                    ..settings_before.clone()
                },
                Some(credentials),
            )
            .await
            .unwrap_err();

        assert!(matches!(error, AppError::InvalidData(_)), "{error:?}");
        assert_eq!(
            serde_json::to_value(service.settings().await).unwrap(),
            serde_json::to_value(settings_before).unwrap()
        );
        assert_eq!(
            load_versioned_identity(backend.as_ref())
                .await
                .unwrap()
                .protection,
            remote_protection
        );
        for kind in [
            SecretKind::S3AccessKeyId,
            SecretKind::S3SecretAccessKey,
            SecretKind::S3SessionToken,
            SecretKind::SyncPassphrase,
        ] {
            assert!(
                service
                    .credentials
                    .get(&remote_id, kind)
                    .await
                    .unwrap()
                    .is_none(),
                "mismatched join persisted {kind:?}"
            );
        }
    }
}

#[test]
fn cloud_error_kinds_map_to_distinct_runtime_states() {
    let cases = [
        (CloudErrorKind::Auth, CloudSyncState::AuthError, "auth"),
        (CloudErrorKind::Offline, CloudSyncState::Offline, "offline"),
        (
            CloudErrorKind::Precondition,
            CloudSyncState::ProtocolError,
            "precondition",
        ),
        (
            CloudErrorKind::Protocol,
            CloudSyncState::ProtocolError,
            "protocol",
        ),
    ];
    for (kind, expected_state, expected_code) in cases {
        let error = map_cloud_error(CloudError::new(kind, "fixture cloud error"));
        assert_eq!(
            classify_cloud_error(&error),
            (expected_state, expected_code)
        );
    }
    assert_eq!(
        classify_cloud_error(&crate::error::AppError::Credential("missing".into())),
        (CloudSyncState::NeedsUnlock, "needs_unlock")
    );
}

#[test]
fn production_scheduler_coalesces_priority_and_uses_bounded_delays() {
    let now = tokio::time::Instant::now();
    let mut worker = CloudSyncWorkerState::default();

    assert!(worker.submit(SyncTrigger::Periodic, now));
    assert_eq!(
        worker.pending_delay(now),
        Some(std::time::Duration::from_secs(15 * 60))
    );
    assert!(worker.submit(SyncTrigger::Startup, now));
    assert!(worker.submit(SyncTrigger::LocalMutation, now));
    assert_eq!(worker.pending_trigger(), Some(SyncTrigger::LocalMutation));
    assert_eq!(
        worker.pending_delay(now),
        Some(std::time::Duration::from_secs(5))
    );

    assert!(worker.submit(SyncTrigger::Manual, now));
    assert_eq!(worker.pending_trigger(), Some(SyncTrigger::Manual));
    assert_eq!(worker.pending_delay(now), Some(std::time::Duration::ZERO));
}

#[test]
fn production_scheduler_pauses_auth_retries_until_manual_trigger() {
    let now = tokio::time::Instant::now();
    let mut worker = CloudSyncWorkerState::default();

    worker.submit(SyncTrigger::Periodic, now);
    let trigger = worker
        .take_due(now + std::time::Duration::from_secs(15 * 60))
        .expect("periodic trigger should become runnable");
    worker.failure(trigger, now, true, false, 17);
    assert!(worker.scheduler.paused_for_auth);
    assert_eq!(worker.pending_trigger(), None);
    assert!(!worker.submit(SyncTrigger::Periodic, now));

    assert!(worker.submit(SyncTrigger::Manual, now));
    assert!(!worker.scheduler.paused_for_auth);
    assert_eq!(worker.pending_delay(now), Some(std::time::Duration::ZERO));
}

#[test]
fn production_scheduler_retries_only_offline_with_capped_jitter() {
    let now = tokio::time::Instant::now();
    let mut worker = CloudSyncWorkerState::default();

    worker.submit(SyncTrigger::Startup, now);
    let trigger = worker
        .take_due(now + std::time::Duration::from_secs(30))
        .expect("startup trigger should become runnable");
    worker.failure(trigger, now, false, true, 0);
    let retry = worker
        .pending_delay(now)
        .expect("offline failure should schedule a retry");
    assert!(retry >= std::time::Duration::from_secs(48));
    assert!(retry <= std::time::Duration::from_secs(72));

    for _ in 0..10 {
        let trigger = worker
            .take_due(now + std::time::Duration::from_secs(2 * 60 * 60))
            .expect("retry should become runnable");
        worker.failure(trigger, now, false, true, 0);
    }
    assert_eq!(
        worker.scheduler.retry_delay,
        std::time::Duration::from_secs(60 * 60)
    );

    let trigger = worker
        .take_due(now + std::time::Duration::from_secs(2 * 60 * 60))
        .expect("capped retry should become runnable");
    worker.failure(trigger, now, false, false, 0);
    assert_eq!(worker.pending_trigger(), None);
}

#[tokio::test]
async fn enabling_cloud_sync_queues_the_seeded_baseline_for_automatic_sync() {
    let mut service = service_with_local_session().await;
    let (sender, mut receiver) = mpsc::channel(1);
    service.cloud_sync_scheduler = CloudSyncScheduler { sender };
    let server = TestS3::start("AKID", None).await;
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "automatic-baseline".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };

    service
        .update_settings_with_cloud_credentials(
            settings,
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: None,
                sync_password: None,
            }),
        )
        .await
        .unwrap();

    assert!(matches!(
        receiver.try_recv(),
        Ok(CloudSyncCommand::Trigger(SyncTrigger::LocalMutation))
    ));
}

#[test]
fn backend_switch_rotates_identity_once() {
    let previous = CloudSyncSettings::default();
    let mut s3 = previous.clone();
    s3.backend = CloudBackendKind::S3;

    assert!(prepare_cloud_sync_transition(&previous, &mut s3));
    assert_ne!(s3.remote_id, previous.remote_id);
    assert_ne!(s3.vault_id, previous.vault_id);
    assert_ne!(s3.generation_id, previous.generation_id);

    let switched = s3.clone();
    assert!(!prepare_cloud_sync_transition(&switched, &mut s3));
    assert_eq!(s3.remote_id, switched.remote_id);
    assert_eq!(s3.vault_id, switched.vault_id);
    assert_eq!(s3.generation_id, switched.generation_id);
}

#[test]
fn changing_remote_location_rotates_identity_and_requests_a_new_baseline() {
    let mut previous = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        ..CloudSyncSettings::default()
    };
    previous.s3.bucket = "first-bucket".into();
    let mut moved = previous.clone();
    moved.s3.bucket = "second-bucket".into();

    assert!(prepare_cloud_sync_transition(&previous, &mut moved));
    assert_ne!(moved.remote_id, previous.remote_id);
    assert_ne!(moved.vault_id, previous.vault_id);
    assert_ne!(moved.generation_id, previous.generation_id);
}

#[test]
fn unverified_new_or_changed_connection_cannot_be_enabled() {
    let previous = CloudSyncSettings::default();
    let mut forged = previous.clone();
    forged.enabled = true;
    forged.connection_verified = true;
    assert!(validate_cloud_sync_update(&previous, &mut forged, false).is_err());
    assert!(!forged.connection_verified);

    let mut enabled = previous.clone();
    enabled.enabled = true;
    assert!(validate_cloud_sync_update(&previous, &mut enabled, false).is_err());

    enabled.connection_verified = true;
    assert!(validate_cloud_sync_update(&previous, &mut enabled, true).is_ok());

    let mut changed = enabled.clone();
    changed.s3.bucket = "another-bucket".into();
    changed.backend = CloudBackendKind::S3;
    assert!(validate_cloud_sync_update(&enabled, &mut changed, false).is_err());
    assert!(!changed.connection_verified);
}

#[test]
fn enabled_legacy_webdav_connection_remains_usable() {
    let previous = CloudSyncSettings {
        enabled: true,
        ..CloudSyncSettings::default()
    };
    let mut next = previous.clone();

    assert!(validate_cloud_sync_update(&previous, &mut next, false).is_ok());
    assert!(next.connection_verified);
}

#[tokio::test]
async fn import_waits_for_generation_maintenance_before_mutating_local_state() {
    let service = service_with_local_session().await;
    let maintenance = service.sync_gate.lock().await;
    let started = Arc::new(Notify::new());
    let mut operation = tokio::spawn({
        let service = service.clone();
        let started = started.clone();
        async move {
            started.notify_one();
            service
                .import(ImportRequest {
                    platform: "fixture".into(),
                    sessions: vec![serde_json::json!({
                        "id": "local-2",
                        "title": "Imported during maintenance",
                        "messages": []
                    })],
                })
                .await
        }
    });
    started.notified().await;

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut operation)
            .await
            .is_err(),
        "import completed while generation maintenance held the gate"
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&service.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    drop(maintenance);
    tokio::time::timeout(std::time::Duration::from_secs(2), &mut operation)
        .await
        .expect("import remained blocked after maintenance finished")
        .unwrap()
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&service.pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn delete_waits_for_generation_maintenance_before_mutating_local_state() {
    let service = service_with_local_session().await;
    let maintenance = service.sync_gate.lock().await;
    let started = Arc::new(Notify::new());
    let mut operation = tokio::spawn({
        let service = service.clone();
        let started = started.clone();
        async move {
            started.notify_one();
            service.delete("local-session").await
        }
    });
    started.notified().await;

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut operation)
            .await
            .is_err(),
        "delete completed while generation maintenance held the gate"
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&service.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    drop(maintenance);
    tokio::time::timeout(std::time::Duration::from_secs(2), &mut operation)
        .await
        .expect("delete remained blocked after maintenance finished")
        .unwrap()
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&service.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn backend_switch_waits_for_running_sync_before_changing_configuration() {
    let service = service_with_local_session().await;
    let running_sync = service.sync_gate.lock().await;
    let mut next = service.settings().await;
    next.cloud_sync.backend = CloudBackendKind::S3;
    next.cloud_sync.s3.bucket = "new-backend".into();

    let transition_started = Arc::new(Notify::new());
    let mut transition = tokio::spawn({
        let service = service.clone();
        let transition_started = transition_started.clone();
        async move {
            transition_started.notify_one();
            service
                .update_settings_with_cloud_credentials(next, None)
                .await
        }
    });
    transition_started.notified().await;

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut transition)
            .await
            .is_err(),
        "backend switch completed while the sync gate was still held"
    );
    assert_eq!(
        service.settings().await.cloud_sync.backend,
        CloudBackendKind::Webdav
    );
    assert!(
        service
            .sync_store
            .pending_mutations(10)
            .await
            .unwrap()
            .is_empty()
    );

    drop(running_sync);
    let updated = tokio::time::timeout(std::time::Duration::from_secs(10), &mut transition)
        .await
        .expect("backend switch remained blocked after the running sync completed")
        .unwrap()
        .unwrap();

    assert_eq!(updated.cloud_sync.backend, CloudBackendKind::S3);
    assert!(
        service
            .sync_store
            .pending_mutations(10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn remove_cloud_device_record_deletes_only_the_requested_remote_prefix() {
    let service = service_with_local_session().await;
    let server = TestS3::start("AKID", None).await;
    let remote_id = format!("remove-test-{}", uuid::Uuid::new_v4().simple());
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: remote_id.clone(),
        vault_id: "vault-remove-test".into(),
        generation_id: "generation-remove-test".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "service-remove".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    service
        .credentials
        .set(
            &remote_id,
            SecretKind::S3AccessKeyId,
            SecretValue::new("AKID"),
        )
        .await
        .unwrap();
    service
        .credentials
        .set(
            &remote_id,
            SecretKind::S3SecretAccessKey,
            SecretValue::new("secret-key"),
        )
        .await
        .unwrap();
    service
        .sync_store
        .initialize_device("device-local", "本机")
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, service.credentials.as_ref())
        .await
        .unwrap();
    let remote_head = crate::sync::backend::RemotePath::parse(
        "v1/generations/generation-remove-test/devices/device-remote/head.json",
    )
    .unwrap();
    backend
        .put_if_absent(&remote_head, b"fixture")
        .await
        .unwrap();

    service
        .remove_cloud_device_record("device-remote".into())
        .await
        .unwrap();

    assert_eq!(
        backend.get(&remote_head).await.unwrap_err().kind(),
        "not_found"
    );
    assert!(
        service
            .remove_cloud_device_record("device-local".into())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn sync_adopts_a_remote_generation_and_replays_the_local_baseline_once() {
    let service = service_with_local_session().await;
    let server = TestS3::start("AKID", None).await;
    let remote_id = format!("adoption-test-{}", uuid::Uuid::new_v4().simple());
    let old_generation = "generation-old".to_owned();
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: remote_id.clone(),
        vault_id: "vault-adoption-test".into(),
        generation_id: old_generation.clone(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "service-adoption".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    service
        .credentials
        .set(
            &remote_id,
            SecretKind::S3AccessKeyId,
            SecretValue::new("AKID"),
        )
        .await
        .unwrap();
    service
        .credentials
        .set(
            &remote_id,
            SecretKind::S3SecretAccessKey,
            SecretValue::new("secret-key"),
        )
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, service.credentials.as_ref())
        .await
        .unwrap();
    let old_identity = VaultIdentity {
        format_version: 2,
        vault_id: settings.cloud_sync.vault_id.clone(),
        generation_id: old_generation.clone(),
    };
    load_or_create_identity(backend.as_ref(), old_identity.clone())
        .await
        .unwrap();

    service.sync_once_locked(settings.clone()).await.unwrap();
    let device = service.ensure_local_device().await.unwrap();
    let old_head = RemotePath::parse(&format!(
        "v1/generations/{old_generation}/devices/{}/head.json",
        device.device_id
    ))
    .unwrap();
    assert!(backend.get(&old_head).await.is_ok());
    let version_before: (i64, i64, String) = sqlx::query_as(
        "SELECT version_wall_ms, version_counter, version_device_id
         FROM sync_entity_versions WHERE platform = 'fixture' AND platform_session_id = 'local-1'",
    )
    .fetch_one(&service.pool)
    .await
    .unwrap();
    let new_identity = VaultIdentity {
        generation_id: "generation-new".into(),
        ..old_identity.clone()
    };
    replace_identity(backend.as_ref(), &old_identity, new_identity.clone())
        .await
        .unwrap();

    service
        .sync_once_locked(service.settings().await)
        .await
        .unwrap();

    assert_eq!(
        service.settings().await.cloud_sync.generation_id,
        new_identity.generation_id
    );
    let publication: (String, String) = sqlx::query_as(
        "SELECT vault_id, generation_id FROM sync_publication_state WHERE singleton = 1",
    )
    .fetch_one(&service.pool)
    .await
    .unwrap();
    assert_eq!(
        publication,
        (
            new_identity.vault_id.clone(),
            new_identity.generation_id.clone()
        )
    );
    assert_eq!(
        sqlx::query_as::<_, (i64, i64, String)>(
            "SELECT version_wall_ms, version_counter, version_device_id
             FROM sync_entity_versions WHERE platform = 'fixture' AND platform_session_id = 'local-1'",
        )
        .fetch_one(&service.pool)
        .await
        .unwrap(),
        version_before
    );
    assert_eq!(
        service.sync_store.pending_mutation_count().await.unwrap(),
        0
    );
    assert!(backend.get(&old_head).await.is_ok());
    let new_head = RemotePath::parse(&format!(
        "v1/generations/{}/devices/{}/head.json",
        new_identity.generation_id, device.device_id
    ))
    .unwrap();
    assert!(backend.get(&new_head).await.is_ok());
    let next_seq_after_adoption: i64 =
        sqlx::query_scalar("SELECT next_seq FROM sync_device_state WHERE singleton = 1")
            .fetch_one(&service.pool)
            .await
            .unwrap();

    service
        .sync_once_locked(service.settings().await)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT next_seq FROM sync_device_state WHERE singleton = 1")
            .fetch_one(&service.pool)
            .await
            .unwrap(),
        next_seq_after_adoption
    );
}

#[tokio::test]
async fn sync_recovers_an_abandoned_frozen_vault_before_publishing() {
    let service = service_with_local_session().await;
    let server = TestS3::start("AKID", None).await;
    let remote_id = format!("frozen-test-{}", uuid::Uuid::new_v4().simple());
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: remote_id.clone(),
        vault_id: "vault-frozen-test".into(),
        generation_id: "generation-old".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "service-frozen-recovery".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    service
        .credentials
        .set(
            &remote_id,
            SecretKind::S3AccessKeyId,
            SecretValue::new("AKID"),
        )
        .await
        .unwrap();
    service
        .credentials
        .set(
            &remote_id,
            SecretKind::S3SecretAccessKey,
            SecretValue::new("secret-key"),
        )
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, service.credentials.as_ref())
        .await
        .unwrap();
    let active = VaultDocument::active(
        VaultIdentity {
            format_version: 2,
            vault_id: settings.cloud_sync.vault_id.clone(),
            generation_id: settings.cloud_sync.generation_id.clone(),
        },
        VaultProtection::plain(),
    );
    load_or_create_vault(backend.as_ref(), active.clone())
        .await
        .unwrap();
    let frozen = begin_generation_freeze_owned(
        backend.as_ref(),
        &active,
        "generation-abandoned",
        VaultProtection::plain(),
        "operation-abandoned",
        "device-abandoned",
        1,
        2,
    )
    .await
    .unwrap();
    assert!(matches!(frozen.state, VaultState::Frozen { .. }));

    service.sync_once_locked(settings).await.unwrap();

    let recovered = load_versioned_identity(backend.as_ref()).await.unwrap();
    assert_eq!(recovered.state, VaultState::Active);
    assert_eq!(recovered.identity.generation_id, "generation-old");
    assert_eq!(
        service.sync_store.pending_mutation_count().await.unwrap(),
        0
    );
}

#[tokio::test]
async fn rewrite_cloud_archive_cleans_partial_generation_when_baseline_publish_fails() {
    let service = service_with_local_session().await;
    let server = TestS3::start("AKID", None).await;
    let remote_id = format!("pre-cas-test-{}", uuid::Uuid::new_v4().simple());
    let old_generation = "generation-old".to_owned();
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: remote_id.clone(),
        vault_id: "vault-pre-cas-test".into(),
        generation_id: old_generation.clone(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "service-pre-cas".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    service
        .credentials
        .set(
            &remote_id,
            SecretKind::S3AccessKeyId,
            SecretValue::new("AKID"),
        )
        .await
        .unwrap();
    service
        .credentials
        .set(
            &remote_id,
            SecretKind::S3SecretAccessKey,
            SecretValue::new("secret-key"),
        )
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, service.credentials.as_ref())
        .await
        .unwrap();
    let expected = VaultIdentity {
        format_version: 2,
        vault_id: settings.cloud_sync.vault_id.clone(),
        generation_id: old_generation.clone(),
    };
    load_or_create_identity(backend.as_ref(), expected.clone())
        .await
        .unwrap();
    let old_object =
        crate::sync::backend::RemotePath::parse("v1/generations/generation-old/keep.bin").unwrap();
    backend.put_if_absent(&old_object, b"keep").await.unwrap();
    service.ensure_local_device().await.unwrap();
    service.sync_store.seed_local_baseline().await.unwrap();
    let pending_before = service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();
    server.fail_baseline_get_after(0).await;

    let error = service.rewrite_cloud_archive().await.unwrap_err();

    assert!(matches!(error, crate::error::AppError::Cloud(_)));
    assert_eq!(
        service.settings().await.cloud_sync.generation_id,
        old_generation
    );
    assert_eq!(
        crate::sync::vault::load_versioned_identity(backend.as_ref())
            .await
            .unwrap()
            .identity,
        expected
    );
    assert_eq!(
        service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        pending_before
    );
    assert_eq!(backend.get(&old_object).await.unwrap().bytes, b"keep");
    let generations = backend
        .list_depth_one(&crate::sync::backend::RemotePath::parse("v1/generations").unwrap())
        .await
        .unwrap();
    assert_eq!(
        generations
            .into_iter()
            .filter(|entry| entry.is_collection)
            .map(|entry| entry.name)
            .collect::<Vec<_>>(),
        vec!["generation-old"]
    );
}

#[tokio::test]
async fn rewrite_cloud_archive_keeps_activated_generation_when_followup_sync_fails() {
    let service = service_with_local_session().await;
    let server = TestS3::start("AKID", None).await;
    let remote_id = format!("rollback-test-{}", uuid::Uuid::new_v4().simple());
    let old_generation = "generation-old".to_owned();
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: remote_id.clone(),
        vault_id: "vault-rollback-test".into(),
        generation_id: old_generation.clone(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "service-rollback".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    service
        .credentials
        .set(
            &remote_id,
            SecretKind::S3AccessKeyId,
            SecretValue::new("AKID"),
        )
        .await
        .unwrap();
    service
        .credentials
        .set(
            &remote_id,
            SecretKind::S3SecretAccessKey,
            SecretValue::new("secret-key"),
        )
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, service.credentials.as_ref())
        .await
        .unwrap();
    let expected = VaultIdentity {
        format_version: 2,
        vault_id: settings.cloud_sync.vault_id.clone(),
        generation_id: old_generation.clone(),
    };
    load_or_create_identity(backend.as_ref(), expected.clone())
        .await
        .unwrap();
    let old_object =
        crate::sync::backend::RemotePath::parse("v1/generations/generation-old/keep.bin").unwrap();
    backend.put_if_absent(&old_object, b"keep").await.unwrap();
    service.ensure_local_device().await.unwrap();
    service.sync_store.seed_local_baseline().await.unwrap();
    let pending_before = service
        .sync_store
        .pending_mutations(i64::MAX)
        .await
        .unwrap();
    server.fail_baseline_get_after(2).await;

    let error = service.rewrite_cloud_archive().await.unwrap_err();

    assert!(matches!(error, crate::error::AppError::Cloud(_)));
    let persisted_generation = service.settings().await.cloud_sync.generation_id;
    assert_ne!(persisted_generation, old_generation);
    let remote_identity = crate::sync::vault::load_versioned_identity(backend.as_ref())
        .await
        .unwrap()
        .identity;
    assert_eq!(remote_identity.vault_id, expected.vault_id);
    assert_eq!(remote_identity.generation_id, persisted_generation);
    assert_eq!(
        service
            .sync_store
            .pending_mutations(i64::MAX)
            .await
            .unwrap(),
        pending_before
    );
    assert_eq!(backend.get(&old_object).await.unwrap().bytes, b"keep");
    let generations = backend
        .list_depth_one(&crate::sync::backend::RemotePath::parse("v1/generations").unwrap())
        .await
        .unwrap();
    let generation_names = generations
        .into_iter()
        .filter(|entry| entry.is_collection)
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    assert!(generation_names.contains(&"generation-old".to_owned()));
    assert!(generation_names.contains(&persisted_generation));
}

#[tokio::test]
async fn rewrite_cloud_archive_switches_the_persisted_and_remote_generation() {
    let service = service_with_local_session().await;
    let server = TestS3::start("AKID", None).await;
    let remote_id = format!("rewrite-test-{}", uuid::Uuid::new_v4().simple());
    let old_generation = "generation-old".to_owned();
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: remote_id.clone(),
        vault_id: "vault-rewrite-test".into(),
        generation_id: old_generation.clone(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "service-rewrite".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    service
        .credentials
        .set(
            &remote_id,
            SecretKind::S3AccessKeyId,
            SecretValue::new("AKID"),
        )
        .await
        .unwrap();
    service
        .credentials
        .set(
            &remote_id,
            SecretKind::S3SecretAccessKey,
            SecretValue::new("secret-key"),
        )
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, service.credentials.as_ref())
        .await
        .unwrap();
    load_or_create_identity(
        backend.as_ref(),
        VaultIdentity {
            format_version: 2,
            vault_id: settings.cloud_sync.vault_id.clone(),
            generation_id: old_generation.clone(),
        },
    )
    .await
    .unwrap();

    let result = service.rewrite_cloud_archive().await;
    let updated = service.settings().await;
    let remote = load_or_create_identity(
        backend.as_ref(),
        VaultIdentity {
            format_version: 2,
            vault_id: "ignored".into(),
            generation_id: "ignored".into(),
        },
    )
    .await
    .unwrap();
    service
        .credentials
        .delete(&remote_id, SecretKind::S3AccessKeyId)
        .await
        .unwrap();
    service
        .credentials
        .delete(&remote_id, SecretKind::S3SecretAccessKey)
        .await
        .unwrap();

    result.unwrap();
    assert_ne!(updated.cloud_sync.generation_id, old_generation);
    assert_eq!(remote.vault_id, updated.cloud_sync.vault_id);
    assert_eq!(remote.generation_id, updated.cloud_sync.generation_id);
}

#[tokio::test]
async fn released_v1_archive_bootstraps_plain_despite_stale_encryption_setting() {
    let (service, settings, backend, _server) =
        configured_released_v1_service(true, "legacy-stale-encryption").await;

    service.sync_once_locked(settings).await.unwrap();

    let persisted = service.settings().await;
    assert_eq!(persisted.cloud_sync.vault_id, "default");
    assert_eq!(persisted.cloud_sync.generation_id, "generation-1");
    assert!(!persisted.cloud_sync.encryption_enabled);
    let remote = load_versioned_identity(backend.as_ref()).await.unwrap();
    assert_eq!(remote.protection, VaultProtection::plain());
    let vault_json: serde_json::Value = serde_json::from_slice(
        &backend
            .get(&RemotePath::parse("v1/vault.json").unwrap())
            .await
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(
        vault_json
            .get("compatibility")
            .and_then(|value| value.as_str()),
        Some("released_v1_writers")
    );
    let imported_title: String = sqlx::query_scalar(
        "SELECT title FROM sessions
         WHERE platform = 'legacy' AND platform_session_id = 'remote-only'",
    )
    .fetch_one(&service.pool)
    .await
    .unwrap();
    assert_eq!(imported_title, "released remote only");
}

#[tokio::test]
async fn released_v1_compatibility_fences_encryption_rotation() {
    let (service, settings, backend, _server) =
        configured_released_v1_service(false, "legacy-encryption-fence").await;
    service.sync_once_locked(settings).await.unwrap();
    let before = service.settings().await;
    let mut requested = before.clone();
    requested.cloud_sync.encryption_enabled = true;

    let error = service
        .update_settings_with_cloud_credentials(
            requested,
            Some(CloudCredentialInput::S3 {
                access_key_id: "AKID".into(),
                secret_access_key: "secret-key".into(),
                session_token: None,
                sync_password: Some("replacement-passphrase".into()),
            }),
        )
        .await
        .unwrap_err();

    assert!(
        matches!(error, AppError::Configuration(ref message) if message.contains("重写云端存档")),
        "{error:?}"
    );
    assert_eq!(
        serde_json::to_value(service.settings().await).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    let remote = load_versioned_identity(backend.as_ref()).await.unwrap();
    assert_eq!(remote.identity.generation_id, "generation-1");
    assert_eq!(remote.protection, VaultProtection::plain());
}

#[tokio::test]
async fn rewrite_cloud_archive_explicitly_retires_released_v1_compatibility() {
    let (service, settings, backend, _server) =
        configured_released_v1_service(false, "legacy-explicit-retirement").await;
    service.sync_once_locked(settings).await.unwrap();
    let before = service.settings().await;

    service.rewrite_cloud_archive().await.unwrap();

    let after = service.settings().await;
    assert_ne!(
        after.cloud_sync.generation_id,
        before.cloud_sync.generation_id
    );
    let remote = load_versioned_identity(backend.as_ref()).await.unwrap();
    assert_eq!(
        remote.identity.generation_id,
        after.cloud_sync.generation_id
    );
    let vault_json: serde_json::Value = serde_json::from_slice(
        &backend
            .get(&RemotePath::parse("v1/vault.json").unwrap())
            .await
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert!(vault_json.get("compatibility").is_none());
    assert!(
        backend
            .get(
                &RemotePath::parse(
                    "v1/generations/generation-1/devices/device-released/head.json",
                )
                .unwrap(),
            )
            .await
            .is_ok(),
        "explicit retirement must not delete released history"
    );
}

#[tokio::test]
async fn move_data_directory_rejects_writes_until_restart() {
    // After the DB is snapshotted to the new directory and a restart is
    // scheduled, the service must reject all subsequent writes (import/delete/
    // sync) so nothing mutates the old pool before the process actually
    // restarts.
    let (service, data_dir) = service_with_local_session_fixture().await;
    let destination =
        std::env::temp_dir().join(format!("ai-chat-memory-move-{}", uuid::Uuid::new_v4()));

    service.move_data_directory(&destination).await.unwrap();
    assert!(
        destination.join("chat_memory.db").exists(),
        "snapshot was written"
    );

    // Subsequent local writes must be rejected, not silently executed.
    let delete_err = service.delete("local-session").await.unwrap_err();
    assert!(
        matches!(delete_err, AppError::Cancelled(_)),
        "delete after move must be Cancelled, got {:?}",
        delete_err
    );

    let import_err = service
        .import(ImportRequest {
            platform: "fixture".into(),
            sessions: vec![serde_json::json!({
                "id": "local-2",
                "title": "after move",
                "messages": []
            })],
        })
        .await
        .unwrap_err();
    assert!(
        matches!(import_err, AppError::Cancelled(_)),
        "import after move must be Cancelled, got {:?}",
        import_err
    );

    // A manual sync must not run either; sync_now falls back to sync_now_direct
    // when the scheduler channel is closed (for_tests), so it must surface the
    // same shutdown rejection rather than touching the old pool.
    let sync_err = service.sync_now().await.unwrap_err();
    assert!(
        matches!(sync_err, AppError::Cancelled(_)),
        "sync_now after move must be Cancelled, got {:?}",
        sync_err
    );

    // The original session row is still present in the old pool (nothing wrote
    // through), proving the rejection happened before any mutation.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&service.pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "no writes should have reached the old pool");

    let _ = std::fs::remove_dir_all(&destination);
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn move_data_directory_rechecks_the_destination_inside_the_sync_gate() {
    // 目标存在性检查必须与 VACUUM 同处 sync_gate 临界区内。并发竞争者
    // 在等锁期间创建了目标文件时，加锁后的复检必须给出明确的配置错误，
    // 而不是让 VACUUM INTO 以晦涩的 SQLite 错误失败。
    let (service, data_dir) = service_with_local_session_fixture().await;
    let destination =
        std::env::temp_dir().join(format!("ai-chat-memory-move-race-{}", uuid::Uuid::new_v4()));

    // 先占用 sync_gate，让移动任务阻塞在临界区入口。
    let gate_guard = service.sync_gate.lock().await;
    let gated_service = service.clone();
    let gated_destination = destination.clone();
    let mover =
        tokio::spawn(async move { gated_service.move_data_directory(&gated_destination).await });
    // 等待移动任务阻塞在 sync_gate 上，再模拟竞争者写入目标文件。
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    tokio::fs::create_dir_all(&destination).await.unwrap();
    tokio::fs::write(destination.join("chat_memory.db"), b"competing")
        .await
        .unwrap();
    drop(gate_guard);
    let error = mover
        .await
        .expect("move task must not panic")
        .expect_err("the destination was created while the move waited on the sync gate");

    assert!(
        matches!(error, AppError::Configuration(ref message) if message.contains("已存在")),
        "the recheck inside the sync gate must surface the friendly conflict, got {error:?}"
    );

    let _ = std::fs::remove_dir_all(&destination);
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn rewrite_cloud_archive_surfaces_and_marks_a_failed_local_generation_commit() {
    // 云端已提交新代次后本地 settings 写入失败，必须重试并在彻底失败时
    // 显式报告错配，让下一次同步以远端代次自愈，而不是静默留下代次错配窗口。
    let (service, data_dir) = service_with_local_session_fixture().await;
    let server = TestS3::start("AKID", None).await;
    let remote_id = format!("svc6-test-{}", uuid::Uuid::new_v4().simple());
    let old_generation = "generation-old".to_owned();
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: remote_id.clone(),
        vault_id: "vault-svc6-test".into(),
        generation_id: old_generation.clone(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "service-svc6".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    service
        .credentials
        .set(
            &remote_id,
            SecretKind::S3AccessKeyId,
            SecretValue::new("AKID"),
        )
        .await
        .unwrap();
    service
        .credentials
        .set(
            &remote_id,
            SecretKind::S3SecretAccessKey,
            SecretValue::new("secret-key"),
        )
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, service.credentials.as_ref())
        .await
        .unwrap();
    load_or_create_identity(
        backend.as_ref(),
        VaultIdentity {
            format_version: 2,
            vault_id: settings.cloud_sync.vault_id.clone(),
            generation_id: old_generation.clone(),
        },
    )
    .await
    .unwrap();
    service.ensure_local_device().await.unwrap();
    service.sync_store.seed_local_baseline().await.unwrap();
    // 让 settings.json 的原子写入必然失败（临时文件名被目录占用）。
    tokio::fs::create_dir(data_dir.join("settings.json.tmp"))
        .await
        .unwrap();

    let error = service.rewrite_cloud_archive().await.unwrap_err();

    assert!(
        matches!(error, AppError::Configuration(ref message)
            if message.contains("云端存档已提交新代次") && message.contains("本地设置写入失败")),
        "the failed local commit must surface the compensation context, got {error:?}"
    );
    assert_eq!(
        service.settings().await.cloud_sync.generation_id,
        old_generation,
        "the persisted settings must still hold the stale generation"
    );
    let remote_generation = load_versioned_identity(backend.as_ref())
        .await
        .unwrap()
        .identity
        .generation_id;
    assert_ne!(
        remote_generation, old_generation,
        "the remote generation must have advanced"
    );

    // 补偿语义：解除写入故障后，下一次同步以远端代次自愈本地设置。
    tokio::fs::remove_dir(data_dir.join("settings.json.tmp"))
        .await
        .unwrap();
    service
        .sync_once_locked(service.settings().await)
        .await
        .unwrap();
    assert_eq!(
        service.settings().await.cloud_sync.generation_id,
        remote_generation
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn remove_cloud_device_record_refreshes_devices_without_faking_sync_success() {
    // 删除单个远端设备不是一次完整的云同步成功——不得刷新
    // last_success_at/清空错误状态；且必须枚举删除设备前缀下的全部对象。
    let service = service_with_local_session().await;
    let server = TestS3::start("AKID", None).await;
    let remote_id = format!("svc7-test-{}", uuid::Uuid::new_v4().simple());
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: remote_id.clone(),
        vault_id: "vault-svc7-test".into(),
        generation_id: "generation-svc7-test".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "service-svc7".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    service
        .credentials
        .set(
            &remote_id,
            SecretKind::S3AccessKeyId,
            SecretValue::new("AKID"),
        )
        .await
        .unwrap();
    service
        .credentials
        .set(
            &remote_id,
            SecretKind::S3SecretAccessKey,
            SecretValue::new("secret-key"),
        )
        .await
        .unwrap();
    service
        .sync_store
        .initialize_device("device-local", "本机")
        .await
        .unwrap();
    let backend = backend_from_store(&settings.cloud_sync, service.credentials.as_ref())
        .await
        .unwrap();
    let bundle_sha = "ab".repeat(32);
    let remote_head =
        RemotePath::parse("v1/generations/generation-svc7-test/devices/device-remote/head.json")
            .unwrap();
    let remote_bundle = RemotePath::parse(&format!(
        "v1/generations/generation-svc7-test/devices/device-remote/bundles/1-1-{bundle_sha}.acmb"
    ))
    .unwrap();
    backend
        .put_if_absent(&remote_head, b"fixture")
        .await
        .unwrap();
    backend
        .put_immutable(&remote_bundle, b"stale-bundle")
        .await
        .unwrap();
    service
        .sync_store
        .set_remote_cursor(
            "generation-svc7-test",
            "device-remote",
            &crate::sync::store::RemoteObjectAnchor {
                end_seq: 1,
                path: remote_bundle.display(),
                sha256: bundle_sha,
            },
            42,
        )
        .await
        .unwrap();
    // 预置一次失败的同步状态：删除设备记录不能把它洗成“成功”。
    service
        .mark_cloud_error(&AppError::Cloud(CloudError::new(
            CloudErrorKind::Protocol,
            "previous sync failure",
        )))
        .await;

    let status = service
        .remove_cloud_device_record("device-remote".into())
        .await
        .unwrap();

    assert_eq!(
        status.last_success_at, None,
        "removing a device record must not fabricate a sync success"
    );
    assert_eq!(
        status.last_error_code.as_deref(),
        Some("protocol"),
        "the previous sync error state must be preserved"
    );
    assert!(
        status
            .last_error_message
            .as_deref()
            .is_some_and(|message| message.contains("previous sync failure"))
    );
    assert!(
        !status
            .devices
            .iter()
            .any(|device| device.device_id == "device-remote"),
        "the deleted device must disappear from the refreshed device list"
    );
    assert_eq!(
        backend.get(&remote_head).await.unwrap_err().kind(),
        "not_found"
    );
    assert_eq!(
        backend.get(&remote_bundle).await.unwrap_err().kind(),
        "not_found",
        "the whole device prefix must be enumerated and deleted, not just the head"
    );
    assert!(
        service
            .sync_store
            .remote_cursor("generation-svc7-test", "device-remote")
            .await
            .unwrap()
            .is_none(),
        "the local sync cursor for the deleted device must be removed"
    );
}

#[tokio::test]
async fn remove_cloud_device_record_reports_cursor_cleanup_failures() {
    // 游标清理失败必须显式报错（远端已删、本地游标残留会导致下次
    // 同步以陈旧游标计算拉取起点），而不是静默成功。
    let service = service_with_local_session().await;
    let server = TestS3::start("AKID", None).await;
    let remote_id = format!("svc7-fail-test-{}", uuid::Uuid::new_v4().simple());
    let mut settings = service.settings().await;
    settings.cloud_sync = CloudSyncSettings {
        backend: CloudBackendKind::S3,
        enabled: true,
        connection_verified: true,
        remote_id: remote_id.clone(),
        vault_id: "vault-svc7-fail-test".into(),
        generation_id: "generation-svc7-fail-test".into(),
        s3: S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "service-svc7-fail".into(),
            force_path_style: true,
        },
        ..CloudSyncSettings::default()
    };
    service.settings.update(settings.clone()).await.unwrap();
    service
        .credentials
        .set(
            &remote_id,
            SecretKind::S3AccessKeyId,
            SecretValue::new("AKID"),
        )
        .await
        .unwrap();
    service
        .credentials
        .set(
            &remote_id,
            SecretKind::S3SecretAccessKey,
            SecretValue::new("secret-key"),
        )
        .await
        .unwrap();
    service
        .sync_store
        .initialize_device("device-local", "本机")
        .await
        .unwrap();
    sqlx::query("DROP TABLE sync_remote_cursors")
        .execute(&service.pool)
        .await
        .unwrap();

    let error = service
        .remove_cloud_device_record("device-remote".into())
        .await
        .expect_err("cursor cleanup failure must surface instead of faking success");

    assert!(
        matches!(error, AppError::InvalidData(ref message) if message.contains("游标清理失败")),
        "got {error:?}"
    );
}
