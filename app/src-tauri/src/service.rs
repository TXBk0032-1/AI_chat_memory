use sqlx::SqlitePool;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{Mutex, RwLock, Semaphore, mpsc, oneshot},
    time::Instant,
};
use zeroize::Zeroizing;

use crate::{
    database,
    embedding::EmbeddingManager,
    error::{AppError, Result},
    import_history,
    models::*,
    normalizer,
    semantic::SemanticEngine,
    settings::SettingsStore,
    sync::{
        backend::{CloudBackend, CloudError, RemotePath},
        bundle::{
            BundleLimits, ProtectionAlgorithm, open_bundle,
            open_released_v1_unchained_bundle_protected,
        },
        credentials::{
            CredentialStore, CredentialTransitionPhase, PendingCredentialProfile,
            StoredCloudCredentialProfile, StoredCredentialBundle, SystemCredentialStore,
            delete_credential_bundle, load_credential_bundle, load_or_migrate_credential_bundle,
            save_credential_bundle,
        },
        crypto::PayloadProtector,
        engine::{HeadDocument, RotationOutcome, SchedulerState, SyncEngine},
        factory::{backend_from_input, backend_from_profile, backend_from_store},
        store::{DeviceState, SyncStore},
        types::SyncTrigger,
        vault::{
            GenerationMaintenanceStage, VaultDocument, VaultIdentity, VaultProtection, VaultState,
            VaultUpdateOutcome, VersionedVaultIdentity, activate_frozen_generation_outcome,
            load_or_create_vault, load_versioned_identity, recover_frozen_generation_from_snapshot,
            recover_head_publish,
        },
    },
};

const MAX_HISTORY_ARCHIVE_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone)]
struct CloudSyncRuntime {
    state: CloudSyncState,
    last_success_at: Option<String>,
    last_error_code: Option<String>,
    last_error_message: Option<String>,
    devices: Vec<RemoteDeviceStatus>,
}

impl Default for CloudSyncRuntime {
    fn default() -> Self {
        Self {
            state: CloudSyncState::Disabled,
            last_success_at: None,
            last_error_code: None,
            last_error_message: None,
            devices: Vec::new(),
        }
    }
}

const CLOUD_SYNC_TRIGGER_QUEUE_CAPACITY: usize = 32;

enum CloudSyncCommand {
    Trigger(SyncTrigger),
    Manual {
        reply: oneshot::Sender<Result<CloudSyncStatus>>,
    },
}

#[derive(Clone)]
struct CloudSyncScheduler {
    sender: mpsc::Sender<CloudSyncCommand>,
}

impl CloudSyncScheduler {
    fn production() -> (Self, mpsc::Receiver<CloudSyncCommand>) {
        let (sender, receiver) = mpsc::channel(CLOUD_SYNC_TRIGGER_QUEUE_CAPACITY);
        (Self { sender }, receiver)
    }

    #[cfg(test)]
    fn for_tests() -> Self {
        let (sender, receiver) = mpsc::channel(CLOUD_SYNC_TRIGGER_QUEUE_CAPACITY);
        drop(receiver);
        Self { sender }
    }
}

#[derive(Clone, Copy)]
struct PendingCloudSync {
    trigger: SyncTrigger,
    due: Instant,
}

#[derive(Default)]
struct CloudSyncWorkerState {
    scheduler: SchedulerState,
    pending: Option<PendingCloudSync>,
}

impl CloudSyncWorkerState {
    fn submit(&mut self, trigger: SyncTrigger, now: Instant) -> bool {
        if self.scheduler.paused_for_auth && trigger != SyncTrigger::Manual {
            return false;
        }
        if let Some(current) = self.pending.take() {
            self.scheduler.submit(current.trigger);
        }
        self.scheduler.submit(trigger);
        let Some(selected) = self.scheduler.take() else {
            return false;
        };
        self.pending = Some(PendingCloudSync {
            trigger: selected,
            due: now + SchedulerState::delay_for(selected),
        });
        true
    }

    #[cfg(test)]
    fn pending_trigger(&self) -> Option<SyncTrigger> {
        self.pending.map(|pending| pending.trigger)
    }

    #[cfg(test)]
    fn pending_delay(&self, now: Instant) -> Option<std::time::Duration> {
        self.pending
            .map(|pending| pending.due.saturating_duration_since(now))
    }

    fn pending_due(&self) -> Option<Instant> {
        self.pending.map(|pending| pending.due)
    }

    fn take_due(&mut self, now: Instant) -> Option<SyncTrigger> {
        if self.pending.is_some_and(|pending| pending.due <= now) {
            return self.pending.take().map(|pending| pending.trigger);
        }
        None
    }

    fn success(&mut self) {
        self.scheduler.success();
    }

    fn failure(
        &mut self,
        trigger: SyncTrigger,
        now: Instant,
        authentication: bool,
        retryable: bool,
        entropy: u32,
    ) {
        self.scheduler.failure(authentication);
        if self.scheduler.paused_for_auth || !retryable {
            if !authentication {
                self.scheduler.success();
            }
            self.pending = None;
            return;
        }
        self.pending = Some(PendingCloudSync {
            trigger,
            due: now + self.scheduler.retry_delay_with_jitter(entropy),
        });
    }
}

fn handle_cloud_sync_command(
    worker: &mut CloudSyncWorkerState,
    command: CloudSyncCommand,
    manual_waiters: &mut Vec<oneshot::Sender<Result<CloudSyncStatus>>>,
    now: Instant,
) {
    match command {
        CloudSyncCommand::Trigger(trigger) => {
            worker.submit(trigger, now);
        }
        CloudSyncCommand::Manual { reply } => {
            manual_waiters.push(reply);
            worker.submit(SyncTrigger::Manual, now);
        }
    }
}

fn send_cloud_sync_success(
    manual_waiters: &mut Vec<oneshot::Sender<Result<CloudSyncStatus>>>,
    status: CloudSyncStatus,
) {
    for waiter in manual_waiters.drain(..) {
        let _ = waiter.send(Ok(status.clone()));
    }
}

fn send_cloud_sync_error(
    manual_waiters: &mut Vec<oneshot::Sender<Result<CloudSyncStatus>>>,
    error: AppError,
) {
    let mut waiters = manual_waiters.drain(..);
    if let Some(waiter) = waiters.next() {
        let message = error.to_string();
        let _ = waiter.send(Err(error));
        for waiter in waiters {
            let _ = waiter.send(Err(AppError::Configuration(message.clone())));
        }
    }
}

fn retry_entropy() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos()
}

fn current_epoch_millis() -> Result<i64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AppError::InvalidData("system clock is before the Unix epoch".into()))?
        .as_millis();
    i64::try_from(millis)
        .map_err(|_| AppError::InvalidData("system clock is outside the supported range".into()))
}

/// Which autonomous background responsibilities an [`AppService`] owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceRole {
    /// Desktop GUI: owns every self-scheduled background worker (semantic
    /// indexing, cloud sync scheduling).
    Desktop,
    /// MCP stdio process: a read/query surface only; it must not spawn
    /// autonomous workers that would fight the desktop process over the same
    /// SQLite database and local embedding model.
    McpStdio,
}

#[derive(Clone)]
pub struct AppService {
    pool: SqlitePool,
    settings: Arc<SettingsStore>,
    semantic: Arc<SemanticEngine>,
    role: ServiceRole,
    api_status: Arc<RwLock<ApiStatus>>,
    last_userscript_request_at: Arc<RwLock<Option<u64>>>,
    sync_store: SyncStore,
    credentials: Arc<dyn CredentialStore>,
    cloud_sync_scheduler: CloudSyncScheduler,
    sync_gate: Arc<Mutex<()>>,
    cloud_sync_runtime: Arc<RwLock<CloudSyncRuntime>>,
    /// Set after `move_data_directory` snapshots the database to a new
    /// location. Once set, every write path rejects new work with
    /// `AppError::Cancelled` so nothing mutates the old pool between the
    /// snapshot and the actual process restart. Reads are unaffected; the
    /// cloud sync worker observes this and stops issuing writes.
    shutdown: Arc<AtomicBool>,
}

pub(crate) async fn import_local_sessions(
    pool: &SqlitePool,
    sessions: &[NormalizedSession],
) -> Result<usize> {
    database::import_sessions(pool, sessions, true).await
}

async fn delete_local_session(pool: &SqlitePool, id: &str) -> Result<()> {
    database::delete_session(pool, id, true).await
}

impl AppService {
    pub async fn new(
        pool: SqlitePool,
        settings: Arc<SettingsStore>,
        data_dir: PathBuf,
    ) -> Result<Self> {
        Self::build(pool, settings, data_dir, ServiceRole::Desktop).await
    }

    /// MCP stdio constructor: skips both self-scheduled workers; manual sync
    /// requests are rejected in `sync_now_direct`.
    pub async fn new_for_mcp_stdio(
        pool: SqlitePool,
        settings: Arc<SettingsStore>,
        data_dir: PathBuf,
    ) -> Result<Self> {
        Self::build(pool, settings, data_dir, ServiceRole::McpStdio).await
    }

    async fn build(
        pool: SqlitePool,
        settings: Arc<SettingsStore>,
        data_dir: PathBuf,
        role: ServiceRole,
    ) -> Result<Self> {
        let settings_value = settings.get().await;
        let embeddings =
            EmbeddingManager::from_settings(data_dir.clone(), settings_value.semantic_search)
                .await?;
        crate::database::connection::ensure_embedding_vec_table(
            &pool,
            Some(embeddings.identity().dimensions),
        )
        .await?;
        let identity = embeddings.identity();
        crate::database::connection::activate_embedding_index(
            &pool,
            &identity.backend_id,
            &identity.model_id,
        )
        .await?;
        let semantic = Arc::new(SemanticEngine::new(pool.clone(), data_dir, embeddings));
        if role == ServiceRole::Desktop {
            semantic.start_worker();
        }
        let sync_store = SyncStore::new(pool.clone());
        let credentials: Arc<dyn CredentialStore> =
            Arc::new(SystemCredentialStore::new("ai-chat-memory"));
        let (cloud_sync_scheduler, worker_receiver) = CloudSyncScheduler::production();
        let service = Self {
            pool,
            settings,
            semantic,
            role,
            api_status: Arc::new(RwLock::new(ApiStatus::Starting)),
            last_userscript_request_at: Arc::new(RwLock::new(None)),
            sync_store,
            credentials,
            cloud_sync_scheduler,
            sync_gate: Arc::new(Mutex::new(())),
            cloud_sync_runtime: Arc::new(RwLock::new(CloudSyncRuntime::default())),
            shutdown: Arc::new(AtomicBool::new(false)),
        };
        if role == ServiceRole::Desktop {
            service.start_cloud_sync_worker(worker_receiver);
        }
        Ok(service)
    }

    fn start_cloud_sync_worker(&self, mut receiver: mpsc::Receiver<CloudSyncCommand>) {
        let service = self.clone();
        tokio::spawn(async move {
            let mut worker = CloudSyncWorkerState::default();
            worker.submit(SyncTrigger::Startup, Instant::now());
            let mut manual_waiters = Vec::new();
            loop {
                // Stop issuing syncs once the database has been snapshotted to a
                // new location: the old pool must not be touched before the
                // process restarts.
                if service.shutdown.load(Ordering::SeqCst) {
                    break;
                }
                if let Some(due) = worker.pending_due() {
                    tokio::select! {
                        biased;
                        command = receiver.recv() => {
                            let Some(command) = command else { break };
                            handle_cloud_sync_command(
                                &mut worker,
                                command,
                                &mut manual_waiters,
                                Instant::now(),
                            );
                        }
                        _ = tokio::time::sleep_until(due) => {
                            let Some(trigger) = worker.take_due(Instant::now()) else {
                                continue;
                            };
                            let result = service.sync_now_direct().await;
                            match result {
                                Ok(status) => {
                                    worker.success();
                                    send_cloud_sync_success(&mut manual_waiters, status);
                                    worker.submit(SyncTrigger::Periodic, Instant::now());
                                }
                                Err(error) => {
                                    let state = classify_cloud_error(&error).0;
                                    let authentication = matches!(
                                        state,
                                        CloudSyncState::AuthError | CloudSyncState::NeedsUnlock
                                    );
                                    let retryable = state == CloudSyncState::Offline;
                                    worker.failure(
                                        trigger,
                                        Instant::now(),
                                        authentication,
                                        retryable,
                                        retry_entropy(),
                                    );
                                    send_cloud_sync_error(&mut manual_waiters, error);
                                    if !authentication && !retryable {
                                        worker.submit(SyncTrigger::Periodic, Instant::now());
                                    }
                                }
                            }
                        }
                    }
                } else {
                    let Some(command) = receiver.recv().await else {
                        break;
                    };
                    handle_cloud_sync_command(
                        &mut worker,
                        command,
                        &mut manual_waiters,
                        Instant::now(),
                    );
                }
            }
        });
    }

    fn notify_local_sync(&self) {
        if let Err(error) = self
            .cloud_sync_scheduler
            .sender
            .try_send(CloudSyncCommand::Trigger(SyncTrigger::LocalMutation))
        {
            tracing::debug!(%error, "cloud sync trigger queue is unavailable");
        }
    }

    pub fn current_settings(&self) -> AppSettings {
        self.settings.current()
    }

    pub async fn settings(&self) -> AppSettings {
        self.settings.current()
    }

    pub async fn update_settings_with_cloud_credentials(
        &self,
        mut settings: AppSettings,
        credentials: Option<CloudCredentialInput>,
    ) -> Result<AppSettings> {
        self.ensure_writable()?;
        let _guard = self.sync_gate.lock().await;
        let previous = self
            .reconcile_pending_credential_transition(self.settings.get().await)
            .await?;
        let remote_switched =
            prepare_cloud_sync_transition(&previous.cloud_sync, &mut settings.cloud_sync);
        let requested_encryption = settings.cloud_sync.encryption_enabled;
        if credentials.is_none()
            && same_cloud_connection(&previous.cloud_sync, &settings.cloud_sync)
            && previous.cloud_sync.encryption_enabled != settings.cloud_sync.encryption_enabled
        {
            return Err(AppError::Configuration(
                "切换云同步加密前必须测试连接并提交当前凭据".into(),
            ));
        }
        let backend = match credentials.as_ref() {
            Some(credentials) => {
                validate_encryption_credentials(&settings.cloud_sync, credentials)?;
                let backend = backend_from_input(&settings.cloud_sync, credentials)?;
                backend.test_capabilities().await.map_err(map_cloud_error)?;
                Some(backend)
            }
            None => None,
        };
        validate_cloud_sync_update(
            &previous.cloud_sync,
            &mut settings.cloud_sync,
            credentials.is_some(),
        )?;
        let connection_activated =
            settings.cloud_sync.enabled && (remote_switched || !previous.cloud_sync.enabled);
        let same_active_connection =
            settings.cloud_sync.enabled && previous.cloud_sync.enabled && !remote_switched;
        let credential_bundle_before = if credentials.is_some() {
            load_credential_bundle(self.credentials.as_ref(), &settings.cloud_sync.remote_id)
                .await?
        } else {
            None
        };
        let previous_sync_passphrase = if same_active_connection {
            self.active_credential_profile(&previous.cloud_sync)
                .await?
                .sync_passphrase()
                .map(|value| Zeroizing::new(value.to_owned()))
        } else {
            None
        };
        let mut rotation_activated = false;
        let mut preserve_credential_transition = false;
        let mut committed_rotation_operation = None;
        let remote_result = async {
            let Some(backend) = backend.as_ref() else {
                return Ok(());
            };
            if connection_activated {
                let proposed_protection = protection_from_input(
                    &settings.cloud_sync.vault_id,
                    requested_encryption,
                    credentials.as_ref(),
                )
                .await?;
                let proposed = VaultDocument::active(
                    VaultIdentity {
                        format_version: 2,
                        vault_id: settings.cloud_sync.vault_id.clone(),
                        generation_id: settings.cloud_sync.generation_id.clone(),
                    },
                    proposed_protection.clone(),
                );
                let input = credentials.as_ref().ok_or_else(|| {
                    AppError::Credential("cloud credentials are not configured".into())
                })?;
                let (current, _) = self
                    .load_verified_vault(
                        backend.as_ref(),
                        &settings.cloud_sync,
                        VaultVerification {
                            create_if_missing: true,
                            expected_vault_id: None,
                            fence_encryption_enabled: requested_encryption,
                            expected_algorithm: Some(proposed_protection.algorithm),
                            proposed: Some(proposed),
                            passphrase: VaultPassphrase::Provided(sync_password_from_input(
                                input,
                            )),
                        },
                    )
                    .await?;
                settings.cloud_sync.vault_id = current.identity.vault_id;
                settings.cloud_sync.generation_id = current.identity.generation_id;
                settings.cloud_sync.encryption_enabled =
                    protection_is_encrypted(&current.protection);
                return Ok(());
            }
            if !same_active_connection {
                return Ok(());
            }

            let (current, _) = self
                .load_verified_vault(
                    backend.as_ref(),
                    &previous.cloud_sync,
                    VaultVerification {
                        create_if_missing: false,
                        expected_vault_id: Some(&previous.cloud_sync.vault_id),
                        fence_encryption_enabled: previous.cloud_sync.encryption_enabled,
                        expected_algorithm: None,
                        proposed: None,
                        passphrase: VaultPassphrase::Stored,
                    },
                )
                .await?;
            settings.cloud_sync.vault_id = current.identity.vault_id.clone();
            settings.cloud_sync.generation_id = current.identity.generation_id.clone();

            let candidate_matches_current =
                if protection_is_encrypted(&current.protection) && requested_encryption {
                    let passphrase = credentials
                        .as_ref()
                        .and_then(sync_password_from_input)
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| {
                            AppError::Configuration("启用包加密时必须填写同步密码".into())
                        })?;
                    match derive_vault_protector(
                        &current.protection,
                        &current.identity.vault_id,
                        passphrase,
                    )
                    .await
                    {
                        Ok(_) => true,
                        Err(AppError::Crypto(_)) => false,
                        Err(error) => return Err(error),
                    }
                } else {
                    false
                };
            let current_encrypted = protection_is_encrypted(&current.protection);
            let encryption_rotated = current_encrypted != requested_encryption
                || (current_encrypted && !candidate_matches_current);
            if !encryption_rotated {
                settings.cloud_sync.encryption_enabled = current_encrypted;
                return Ok(());
            }
            if current.released_v1_compatibility_active() {
                return Err(AppError::Configuration(
                    "旧版同步兼容仍在生效；请先使用“重写云端存档”显式结束兼容，再切换加密"
                        .into(),
                ));
            }

            let old_protector = if current_encrypted {
                let passphrase = previous_sync_passphrase
                    .as_ref()
                    .ok_or_else(|| AppError::Credential("sync passphrase is missing".into()))?;
                derive_vault_protector(
                    &current.protection,
                    &current.identity.vault_id,
                    passphrase.as_str(),
                )
                .await?
            } else {
                None
            };
            let new_protection = protection_from_input(
                &current.identity.vault_id,
                requested_encryption,
                credentials.as_ref(),
            )
            .await?;
            let new_protector = protector_from_input(
                &current.identity.vault_id,
                &new_protection,
                credentials.as_ref(),
            )
            .await?;
            let device = self.ensure_local_device().await?;
            self.sync_store.seed_local_baseline().await?;
            let new_generation = format!("generation-{}", uuid::Uuid::new_v4().simple());
            let operation_id = format!("rotation-{}", uuid::Uuid::new_v4().simple());
            let candidate =
                StoredCloudCredentialProfile::from_input(credentials.as_ref().ok_or_else(
                    || AppError::Credential("replacement cloud credentials are missing".into()),
                )?);
            let mut transition_bundle =
                load_or_migrate_credential_bundle(self.credentials.as_ref(), &previous.cloud_sync)
                    .await?
                    .ok_or_else(|| {
                        AppError::Credential("cloud credentials are not configured".into())
                    })?;
            transition_bundle.stage_transition(PendingCredentialProfile {
                credentials: candidate,
                operation_id: operation_id.clone(),
                target_vault_id: current.identity.vault_id.clone(),
                target_generation_id: new_generation.clone(),
                phase: CredentialTransitionPhase::Prepared,
            })?;
            save_credential_bundle(
                self.credentials.as_ref(),
                &settings.cloud_sync.remote_id,
                &transition_bundle,
            )
            .await?;
            let old_generation = current.identity.generation_id.clone();
            let vault_id = current.identity.vault_id.clone();
            let engine = SyncEngine::new_protected_with_policy(
                self.sync_store.clone(),
                backend.clone(),
                &vault_id,
                &old_generation,
                device.device_id,
                current.protection,
                old_protector,
            )
            .with_semantic(Some(self.semantic.clone()));
            match engine
                .rotate_generation_with_operation(
                    &new_generation,
                    new_protection.clone(),
                    new_protector,
                    &operation_id,
                )
                .await
            {
                RotationOutcome::Committed {
                    operation_id: committed_operation,
                    vault,
                    ..
                } if committed_operation == operation_id
                    && vault.state == VaultState::Active
                    && vault.identity.vault_id == vault_id
                    && vault.identity.generation_id == new_generation =>
                {
                    preserve_credential_transition = true;
                    transition_bundle.set_pending_phase(
                        &operation_id,
                        CredentialTransitionPhase::RemoteCommitted,
                    )?;
                    save_credential_bundle(
                        self.credentials.as_ref(),
                        &settings.cloud_sync.remote_id,
                        &transition_bundle,
                    )
                    .await?;
                }
                RotationOutcome::RolledBack {
                    operation_id: rolled_back_operation,
                    vault,
                    error,
                } if rolled_back_operation == operation_id
                    && vault.state == VaultState::Active
                    && vault.identity.vault_id == vault_id
                    && vault.identity.generation_id == old_generation =>
                {
                    transition_bundle.discard_pending(&operation_id)?;
                    save_credential_bundle(
                        self.credentials.as_ref(),
                        &settings.cloud_sync.remote_id,
                        &transition_bundle,
                    )
                    .await?;
                    return Err(error);
                }
                RotationOutcome::Committed { .. } | RotationOutcome::RolledBack { .. } => {
                    preserve_credential_transition = true;
                    return Err(AppError::InvalidData(
                        "generation rotation outcome does not match the prepared credential transition"
                            .into(),
                    ));
                }
                RotationOutcome::Unknown { error, .. } => {
                    preserve_credential_transition = true;
                    return Err(error);
                }
            }
            rotation_activated = true;
            committed_rotation_operation = Some(operation_id);
            settings.cloud_sync.generation_id = new_generation;
            settings.cloud_sync.encryption_enabled = protection_is_encrypted(&new_protection);
            Ok(())
        }
        .await;
        if let Err(error) = remote_result {
            if credentials.is_some() && !preserve_credential_transition {
                return Err(self
                    .restore_credential_bundle_after_failure(
                        &settings.cloud_sync.remote_id,
                        credential_bundle_before.as_ref(),
                        error,
                    )
                    .await);
            }
            return Err(error);
        }

        if let Some(credentials) = credentials.as_ref()
            && !rotation_activated
        {
            let bundle =
                StoredCredentialBundle::new(StoredCloudCredentialProfile::from_input(credentials));
            if let Err(error) = save_credential_bundle(
                self.credentials.as_ref(),
                &settings.cloud_sync.remote_id,
                &bundle,
            )
            .await
            {
                return Err(self
                    .restore_credential_bundle_after_failure(
                        &settings.cloud_sync.remote_id,
                        credential_bundle_before.as_ref(),
                        error,
                    )
                    .await);
            }
        }

        let updated = match self.settings.update(settings.clone()).await {
            Ok(updated) => updated,
            Err(error) => {
                if credentials.is_some() && !rotation_activated {
                    return Err(self
                        .restore_credential_bundle_after_failure(
                            &settings.cloud_sync.remote_id,
                            credential_bundle_before.as_ref(),
                            error,
                        )
                        .await);
                }
                return Err(error);
            }
        };
        if let Some(operation_id) = committed_rotation_operation.as_deref() {
            let mut bundle =
                load_credential_bundle(self.credentials.as_ref(), &updated.cloud_sync.remote_id)
                    .await?
                    .ok_or_else(|| {
                        AppError::Credential(
                            "cloud credential transition is missing after commit".into(),
                        )
                    })?;
            bundle.promote_pending(operation_id)?;
            save_credential_bundle(
                self.credentials.as_ref(),
                &updated.cloud_sync.remote_id,
                &bundle,
            )
            .await?;
        }
        // The settings are committed and the credential rotation (if any) is
        // finalized. Drop the sync_gate before reloading embeddings and seeding
        // the baseline: a model download/reindex can take tens of seconds and
        // holding the gate across it would block all local writes.
        // rollback_settings_and_credentials only touches SettingsStore + the
        // credential store, neither of which takes sync_gate, so rollback still
        // works once the guard is released.
        drop(_guard);
        if previous.semantic_search != updated.semantic_search
            && let Err(error) = self
                .semantic
                .reload_embeddings(updated.semantic_search.clone())
                .await
        {
            if rotation_activated {
                return Err(error);
            }
            return Err(self
                .rollback_settings_and_credentials(
                    &previous,
                    &settings.cloud_sync.remote_id,
                    credentials.is_some() && !rotation_activated,
                    credential_bundle_before.as_ref(),
                    error,
                )
                .await);
        }
        if connection_activated && updated.cloud_sync.enabled {
            let baseline_result = async {
                self.ensure_local_device().await?;
                self.sync_store.seed_local_baseline().await?;
                Ok::<(), AppError>(())
            }
            .await;
            if let Err(error) = baseline_result {
                return Err(self
                    .rollback_settings_and_credentials(
                        &previous,
                        &settings.cloud_sync.remote_id,
                        credentials.is_some() && !rotation_activated,
                        credential_bundle_before.as_ref(),
                        error,
                    )
                    .await);
            }
        }
        if updated.cloud_sync.enabled && (connection_activated || rotation_activated) {
            self.notify_local_sync();
        }
        Ok(updated)
    }

    pub async fn cloud_sync_status(&self) -> CloudSyncStatus {
        let settings = self.settings().await;
        let pending = self.sync_store.pending_mutation_count().await.unwrap_or(0);
        let runtime = self.cloud_sync_runtime.read().await.clone();
        if !settings.cloud_sync.enabled {
            return CloudSyncStatus {
                state: CloudSyncState::Disabled,
                last_success_at: runtime.last_success_at,
                pending_mutations: pending,
                last_error_code: None,
                last_error_message: None,
                devices: Vec::new(),
            };
        }
        CloudSyncStatus {
            state: if runtime.state == CloudSyncState::Disabled {
                CloudSyncState::Idle
            } else {
                runtime.state
            },
            last_success_at: runtime.last_success_at,
            pending_mutations: pending,
            last_error_code: runtime.last_error_code,
            last_error_message: runtime.last_error_message,
            devices: runtime.devices,
        }
    }

    pub async fn test_cloud_sync_connection(
        &self,
        mut cloud_sync: CloudSyncSettings,
        credentials: CloudCredentialInput,
    ) -> Result<CloudConnectionTestResult> {
        cloud_sync.normalize();
        validate_encryption_credentials(&cloud_sync, &credentials)?;
        let backend = backend_from_input(&cloud_sync, &credentials)?;
        backend.test_capabilities().await.map_err(map_cloud_error)?;
        let mut prepared_cloud_sync = cloud_sync;
        prepare_cloud_sync_transition(&self.settings().await.cloud_sync, &mut prepared_cloud_sync);
        prepared_cloud_sync.connection_verified = true;
        Ok(CloudConnectionTestResult {
            ok: true,
            message: "连接成功".into(),
            supports_conditional_write: true,
            cloud_sync: prepared_cloud_sync,
        })
    }

    async fn restore_credential_bundle(
        &self,
        remote_id: &str,
        before: Option<&StoredCredentialBundle>,
    ) -> Result<()> {
        match before {
            Some(bundle) => {
                save_credential_bundle(self.credentials.as_ref(), remote_id, bundle).await
            }
            None => delete_credential_bundle(self.credentials.as_ref(), remote_id).await,
        }
    }

    async fn restore_credential_bundle_after_failure(
        &self,
        remote_id: &str,
        before: Option<&StoredCredentialBundle>,
        error: AppError,
    ) -> AppError {
        match self.restore_credential_bundle(remote_id, before).await {
            Ok(()) => error,
            Err(rollback) => transaction_rollback_error(&error, rollback),
        }
    }

    async fn rollback_settings_and_credentials(
        &self,
        previous: &AppSettings,
        remote_id: &str,
        restore_credential_bundle: bool,
        credential_bundle_before: Option<&StoredCredentialBundle>,
        error: AppError,
    ) -> AppError {
        let mut rollback_error = self.settings.update(previous.clone()).await.err();
        if restore_credential_bundle
            && let Err(credential_error) = self
                .restore_credential_bundle(remote_id, credential_bundle_before)
                .await
            && rollback_error.is_none()
        {
            rollback_error = Some(credential_error);
        }
        match rollback_error {
            Some(rollback) => transaction_rollback_error(&error, rollback),
            None => error,
        }
    }
}

fn sync_password_from_input(credentials: &CloudCredentialInput) -> Option<&str> {
    match credentials {
        CloudCredentialInput::Webdav { sync_password, .. }
        | CloudCredentialInput::S3 { sync_password, .. } => sync_password.as_deref(),
    }
}

fn protection_is_encrypted(protection: &VaultProtection) -> bool {
    protection.algorithm == ProtectionAlgorithm::XChaCha20Poly1305
}

async fn detect_released_v1_archive(backend: &dyn CloudBackend) -> Result<bool> {
    use sha2::{Digest, Sha256};

    let devices_path = RemotePath::parse("v1/generations/generation-1/devices")
        .map_err(|error| AppError::InvalidData(error.to_string()))?;
    let entries = match backend.list_depth_one(&devices_path).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == "not_found" => return Ok(false),
        Err(error) => return Err(map_cloud_error(error)),
    };
    for entry in entries.into_iter().filter(|entry| entry.is_collection) {
        let head_path = devices_path
            .join(&entry.name)
            .and_then(|path| path.join("head.json"))
            .map_err(|error| AppError::InvalidData(error.to_string()))?;
        let head_object = match backend.get(&head_path).await {
            Ok(object) => object,
            Err(error) if error.kind() == "not_found" => continue,
            Err(error) => return Err(map_cloud_error(error)),
        };
        let head: HeadDocument = serde_json::from_slice(&head_object.bytes)?;
        let expected_prefix = format!(
            "v1/generations/generation-1/devices/{}/bundles/",
            entry.name
        );
        if head.generation_id != "generation-1"
            || head.device_id != entry.name
            || head.end_seq < 1
            || !head.path.starts_with(&expected_prefix)
            || head.sha256.len() != 64
            || !head.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(AppError::InvalidData(
                "released v1 archive head is invalid".into(),
            ));
        }
        let bundle_path = RemotePath::parse(&head.path)
            .map_err(|error| AppError::InvalidData(error.to_string()))?;
        let bundle = backend.get(&bundle_path).await.map_err(map_cloud_error)?;
        if hex::encode(Sha256::digest(&bundle.bytes)) != head.sha256 {
            return Err(AppError::InvalidData(
                "released v1 archive bundle hash is invalid".into(),
            ));
        }
        let decoded = match open_bundle(&bundle.bytes, &BundleLimits::default()) {
            Ok(decoded) => decoded,
            Err(strict_error) => match open_released_v1_unchained_bundle_protected(
                &bundle.bytes,
                &BundleLimits::default(),
                None,
            ) {
                Ok(decoded) => decoded,
                Err(_) => return Err(strict_error),
            },
        };
        if decoded.header.vault_id != "default"
            || decoded.header.generation_id != "generation-1"
            || decoded.header.device_id != entry.name
            || decoded.header.end_seq != head.end_seq
            || decoded.header.protection != ProtectionAlgorithm::Plain
            || decoded.header.previous_path.is_some()
            || decoded.header.previous_sha256.is_some()
            || decoded.header.previous_end_seq.is_some()
        {
            return Err(AppError::InvalidData(
                "released v1 archive bundle identity is invalid".into(),
            ));
        }
        return Ok(true);
    }
    Ok(false)
}

fn kdf_limit() -> Arc<Semaphore> {
    static LIMIT: OnceLock<Arc<Semaphore>> = OnceLock::new();
    LIMIT.get_or_init(|| Arc::new(Semaphore::new(2))).clone()
}

async fn derive_vault_protector(
    protection: &VaultProtection,
    vault_id: &str,
    passphrase: &str,
) -> Result<Option<Arc<dyn PayloadProtector>>> {
    let permit = kdf_limit()
        .acquire_owned()
        .await
        .map_err(|_| AppError::Crypto("sync KDF worker is unavailable".into()))?;
    let protection = protection.clone();
    let vault_id = vault_id.to_owned();
    let passphrase = Zeroizing::new(passphrase.to_owned());
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        protection.derive_protector(&vault_id, passphrase.as_str())
    })
    .await
    .map_err(|_| AppError::Crypto("sync KDF worker failed".into()))?
}

async fn create_encrypted_vault_protection(
    vault_id: &str,
    passphrase: &str,
) -> Result<VaultProtection> {
    let permit = kdf_limit()
        .acquire_owned()
        .await
        .map_err(|_| AppError::Crypto("sync KDF worker is unavailable".into()))?;
    let vault_id = vault_id.to_owned();
    let passphrase = Zeroizing::new(passphrase.to_owned());
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        VaultProtection::encrypted(&vault_id, passphrase.as_str())
    })
    .await
    .map_err(|_| AppError::Crypto("sync KDF worker failed".into()))?
}

async fn protection_from_input(
    vault_id: &str,
    encryption_enabled: bool,
    credentials: Option<&CloudCredentialInput>,
) -> Result<VaultProtection> {
    if !encryption_enabled {
        return Ok(VaultProtection::plain());
    }
    let passphrase = credentials
        .and_then(sync_password_from_input)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::Configuration("启用包加密时必须填写同步密码".into()))?;
    create_encrypted_vault_protection(vault_id, passphrase).await
}

async fn protector_from_input(
    vault_id: &str,
    protection: &VaultProtection,
    credentials: Option<&CloudCredentialInput>,
) -> Result<Option<Arc<dyn PayloadProtector>>> {
    if !protection_is_encrypted(protection) {
        return Ok(None);
    }
    let passphrase = credentials
        .and_then(sync_password_from_input)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::Configuration("启用包加密时必须填写同步密码".into()))?;
    derive_vault_protector(protection, vault_id, passphrase).await
}

#[derive(Clone, Copy)]
enum VaultPassphrase<'a> {
    Stored,
    Provided(Option<&'a str>),
}

struct VaultVerification<'a> {
    create_if_missing: bool,
    expected_vault_id: Option<&'a str>,
    fence_encryption_enabled: bool,
    expected_algorithm: Option<ProtectionAlgorithm>,
    proposed: Option<VaultDocument>,
    passphrase: VaultPassphrase<'a>,
}

fn transaction_rollback_error(error: &AppError, rollback: AppError) -> AppError {
    AppError::InvalidData(format!(
        "{error}; settings transaction rollback failed: {rollback}"
    ))
}

impl AppService {
    pub async fn sync_now(&self) -> Result<CloudSyncStatus> {
        let (reply, response) = oneshot::channel();
        if self
            .cloud_sync_scheduler
            .sender
            .send(CloudSyncCommand::Manual { reply })
            .await
            .is_ok()
        {
            return match response.await {
                Ok(result) => result,
                Err(_) => self.sync_now_direct().await,
            };
        }
        self.sync_now_direct().await
    }

    async fn sync_now_direct(&self) -> Result<CloudSyncStatus> {
        // The MCP stdio process must not touch the cloud archive: it has no
        // scheduled worker, and a manual sync here would race the desktop
        // process over the same generation chain.
        if self.role == ServiceRole::McpStdio {
            return Err(AppError::Configuration(
                "MCP stdio 进程不支持手动云同步：请在桌面应用中执行".into(),
            ));
        }
        self.ensure_writable()?;
        let _guard = self.sync_gate.lock().await;
        let settings = self.settings().await;
        if !settings.cloud_sync.enabled {
            self.cloud_sync_runtime.write().await.state = CloudSyncState::Disabled;
            return Ok(self.cloud_sync_status().await);
        }
        self.mark_cloud_syncing().await;
        match self.sync_once_locked(settings).await {
            Ok(devices) => {
                // Retire `stage='published'` rows older than the retention window so
                // sync_published_bundles does not grow without bound. Recent
                // rows stay for publish idempotency; only stale ones are removed.
                let seven_days_ms: i64 = 7 * 24 * 60 * 60 * 1000;
                let cutoff = chrono::Utc::now().timestamp_millis() - seven_days_ms;
                if let Err(error) = self.sync_store.prune_published_bundles(cutoff).await {
                    tracing::warn!(%error, "failed to prune published bundle records");
                }
                self.mark_cloud_success(devices).await;
                Ok(self.cloud_sync_status().await)
            }
            Err(error) => {
                self.mark_cloud_error(&error).await;
                Err(error)
            }
        }
    }

    async fn sync_once_locked(&self, mut settings: AppSettings) -> Result<Vec<RemoteDeviceStatus>> {
        settings = self
            .reconcile_pending_credential_transition(settings)
            .await?;
        let backend = backend_from_store(&settings.cloud_sync, self.credentials.as_ref()).await?;
        let (remote_vault, protector) = self
            .load_verified_vault(
                backend.as_ref(),
                &settings.cloud_sync,
                VaultVerification {
                    create_if_missing: true,
                    expected_vault_id: Some(&settings.cloud_sync.vault_id),
                    fence_encryption_enabled: settings.cloud_sync.encryption_enabled,
                    expected_algorithm: None,
                    proposed: None,
                    passphrase: VaultPassphrase::Stored,
                },
            )
            .await?;
        let remote_encryption = protection_is_encrypted(&remote_vault.protection);
        if remote_vault.identity.generation_id != settings.cloud_sync.generation_id
            || remote_encryption != settings.cloud_sync.encryption_enabled
        {
            settings.cloud_sync.generation_id = remote_vault.identity.generation_id.clone();
            settings.cloud_sync.encryption_enabled = remote_encryption;
            settings = self.settings.update(settings).await?;
        }
        let device = self.ensure_local_device().await?;
        self.sync_store.seed_local_baseline().await?;
        let engine = SyncEngine::new_protected_with_policy(
            self.sync_store.clone(),
            backend.clone(),
            &remote_vault.identity.vault_id,
            &remote_vault.identity.generation_id,
            device.device_id.clone(),
            remote_vault.protection,
            protector,
        )
        .with_semantic(Some(self.semantic.clone()));
        engine
            .run_once_with_generation_replay(SyncTrigger::Manual)
            .await?;
        self.remote_devices(backend.as_ref(), &settings.cloud_sync, &device)
            .await
    }

    pub async fn rewrite_cloud_archive(&self) -> Result<CloudSyncStatus> {
        let _guard = self.sync_gate.lock().await;
        let settings = self.settings().await;
        if !settings.cloud_sync.enabled {
            return Err(AppError::Configuration("云同步尚未启用".into()));
        }
        self.mark_cloud_syncing().await;
        match self.rewrite_cloud_archive_locked(settings).await {
            Ok(devices) => {
                self.mark_cloud_success(devices).await;
                Ok(self.cloud_sync_status().await)
            }
            Err(error) => {
                self.mark_cloud_error(&error).await;
                Err(error)
            }
        }
    }

    async fn rewrite_cloud_archive_locked(
        &self,
        mut settings: AppSettings,
    ) -> Result<Vec<RemoteDeviceStatus>> {
        settings = self
            .reconcile_pending_credential_transition(settings)
            .await?;
        let backend = backend_from_store(&settings.cloud_sync, self.credentials.as_ref()).await?;
        let (current, protector) = self
            .load_verified_vault(
                backend.as_ref(),
                &settings.cloud_sync,
                VaultVerification {
                    create_if_missing: false,
                    expected_vault_id: Some(&settings.cloud_sync.vault_id),
                    fence_encryption_enabled: settings.cloud_sync.encryption_enabled,
                    expected_algorithm: None,
                    proposed: None,
                    passphrase: VaultPassphrase::Stored,
                },
            )
            .await?;
        let remote_encryption = protection_is_encrypted(&current.protection);
        if settings.cloud_sync.generation_id != current.identity.generation_id
            || settings.cloud_sync.encryption_enabled != remote_encryption
        {
            settings.cloud_sync.generation_id = current.identity.generation_id.clone();
            settings.cloud_sync.encryption_enabled = remote_encryption;
            settings = self.settings.update(settings).await?;
        }
        let device = self.ensure_local_device().await?;
        self.sync_store.seed_local_baseline().await?;
        let new_generation = format!("generation-{}", uuid::Uuid::new_v4().simple());
        let old_generation = current.identity.generation_id.clone();
        let old_engine = SyncEngine::new_protected_with_policy(
            self.sync_store.clone(),
            backend.clone(),
            &current.identity.vault_id,
            &old_generation,
            device.device_id.clone(),
            current.protection.clone(),
            protector.clone(),
        )
        .with_semantic(Some(self.semantic.clone()));
        if let Err(error) = old_engine.rewrite_generation(&new_generation).await {
            self.cleanup_unactivated_generation(backend.as_ref(), &old_generation, &new_generation)
                .await;
            return Err(error);
        }
        settings.cloud_sync.generation_id = new_generation.clone();
        settings.cloud_sync.encryption_enabled = remote_encryption;
        // The cloud generation is already committed at this point, so a
        // failed local settings write must be retried and, if it keeps failing,
        // surfaced as an explicit mismatch for the next sync to repair instead
        // of silently leaving local settings on the old generation.
        let mut last_commit_error = None;
        let mut settings_committed = false;
        for attempt in 0..3 {
            match self.settings.update(settings.clone()).await {
                Ok(_) => {
                    settings_committed = true;
                    break;
                }
                Err(error) => {
                    last_commit_error = Some(error);
                    if attempt + 1 < 3 {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
            }
        }
        if !settings_committed {
            let error = last_commit_error
                .unwrap_or_else(|| AppError::InvalidData("settings update did not run".into()));
            tracing::error!(
                vault_id = %settings.cloud_sync.vault_id,
                remote_generation = %new_generation,
                stale_local_generation = %old_generation,
                error = %error,
                "cloud archive committed a new generation but the local settings commit failed; \
                 the next successful sync adopts the remote generation to repair the mismatch"
            );
            return Err(AppError::Configuration(format!(
                "云端存档已提交新代次，但本地设置写入失败：{error}；下次成功同步会自动校正代次，请重试同步"
            )));
        }

        let new_engine = SyncEngine::new_protected_with_policy(
            self.sync_store.clone(),
            backend.clone(),
            &current.identity.vault_id,
            &new_generation,
            device.device_id.clone(),
            current.protection,
            protector,
        )
        .with_semantic(Some(self.semantic.clone()));
        new_engine
            .run_once_with_generation_replay(SyncTrigger::Manual)
            .await?;
        self.remote_devices(backend.as_ref(), &settings.cloud_sync, &device)
            .await
    }

    async fn cleanup_generation(&self, backend: &dyn CloudBackend, generation: &str) {
        let path = match RemotePath::parse(&format!("v1/generations/{generation}")) {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!(%error, "new cloud generation cleanup path was invalid");
                return;
            }
        };
        if let Err(error) = backend.delete(&path).await {
            tracing::warn!(kind = error.kind(), "new cloud generation cleanup failed");
        }
    }

    async fn cleanup_unactivated_generation(
        &self,
        backend: &dyn CloudBackend,
        previous_generation: &str,
        target_generation: &str,
    ) {
        let current = match load_versioned_identity(backend).await {
            Ok(current) => current,
            Err(error) => {
                tracing::warn!(%error, "could not confirm generation state before cleanup");
                return;
            }
        };
        if current.identity.generation_id == previous_generation {
            self.cleanup_generation(backend, target_generation).await;
        }
    }

    pub async fn remove_cloud_device_record(&self, device_id: String) -> Result<CloudSyncStatus> {
        let _guard = self.sync_gate.lock().await;
        let settings = self
            .reconcile_pending_credential_transition(self.settings().await)
            .await?;
        if !settings.cloud_sync.enabled {
            return Err(AppError::Configuration("云同步尚未启用".into()));
        }
        let local = self.ensure_local_device().await?;
        if device_id == local.device_id || device_id == "baseline" {
            return Err(AppError::Configuration("不能删除当前设备的同步记录".into()));
        }
        let backend = backend_from_store(&settings.cloud_sync, self.credentials.as_ref()).await?;
        let devices_path = RemotePath::parse(&format!(
            "v1/generations/{}/devices",
            settings.cloud_sync.generation_id
        ))
        .map_err(|error| AppError::InvalidData(error.to_string()))?;
        let device_path = devices_path
            .join(&device_id)
            .map_err(|error| AppError::InvalidData(error.to_string()))?;
        backend
            .delete(&device_path)
            .await
            .map_err(map_cloud_error)?;
        // The remote device is already gone, so a cursor cleanup failure
        // must be retried and then reported instead of silently leaving a stale
        // sync_remote_cursors row that would skew the next pull window.
        let mut cursor_error = None;
        for attempt in 0..3 {
            match self
                .sync_store
                .remove_remote_cursor(&settings.cloud_sync.generation_id, &device_id)
                .await
            {
                Ok(()) => {
                    cursor_error = None;
                    break;
                }
                Err(error) => {
                    cursor_error = Some(error);
                    if attempt + 1 < 3 {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
            }
        }
        if let Some(error) = cursor_error {
            return Err(AppError::InvalidData(format!(
                "远端设备记录已删除，但本地同步游标清理失败：{error}；请先重试同步以校正拉取游标"
            )));
        }
        let devices = self
            .remote_devices(backend.as_ref(), &settings.cloud_sync, &local)
            .await?;
        // Deleting a single device record is not a cloud sync success —
        // only refresh the device list and leave state/timestamps/errors as
        // they are instead of calling mark_cloud_success.
        {
            let mut runtime = self.cloud_sync_runtime.write().await;
            runtime.devices = devices;
        }
        Ok(self.cloud_sync_status().await)
    }

    async fn ensure_local_device(&self) -> Result<DeviceState> {
        if let Some(device) = self.sync_store.device_state().await? {
            return Ok(device);
        }
        let device_id = format!("device-{}", uuid::Uuid::new_v4().simple());
        self.sync_store.initialize_device(&device_id, "本机").await
    }

    async fn load_verified_vault(
        &self,
        backend: &dyn CloudBackend,
        settings: &CloudSyncSettings,
        request: VaultVerification<'_>,
    ) -> Result<(VersionedVaultIdentity, Option<Arc<dyn PayloadProtector>>)> {
        let VaultVerification {
            create_if_missing,
            expected_vault_id,
            fence_encryption_enabled,
            expected_algorithm,
            proposed,
            passphrase,
        } = request;
        let mut stored_profile = None;
        let remote = match load_versioned_identity(backend).await {
            Ok(current) => current,
            Err(error) if matches!(&error, AppError::Cloud(cloud) if cloud.kind() == "not_found") =>
            {
                if !create_if_missing {
                    return Err(error);
                }
                let released_v1_archive = detect_released_v1_archive(backend).await?;
                let proposed = if released_v1_archive {
                    VaultDocument::released_v1_compatible(VaultIdentity {
                        format_version: 2,
                        vault_id: "default".into(),
                        generation_id: "generation-1".into(),
                    })
                } else {
                    match proposed {
                        Some(proposed) => proposed,
                        None => {
                            let protection = if settings.encryption_enabled {
                                let profile = self.active_credential_profile(settings).await?;
                                let passphrase = profile.sync_passphrase().ok_or_else(|| {
                                    AppError::Credential("sync passphrase is missing".into())
                                })?;
                                stored_profile = Some(profile.clone());
                                create_encrypted_vault_protection(&settings.vault_id, passphrase)
                                    .await?
                            } else {
                                VaultProtection::plain()
                            };
                            VaultDocument::active(
                                VaultIdentity {
                                    format_version: 2,
                                    vault_id: settings.vault_id.clone(),
                                    generation_id: settings.generation_id.clone(),
                                },
                                protection,
                            )
                        }
                    }
                };
                load_or_create_vault(backend, proposed).await?;
                load_versioned_identity(backend).await?
            }
            Err(error) => return Err(error),
        };
        if let Some(expected_vault_id) = expected_vault_id
            && remote.identity.vault_id != expected_vault_id
        {
            return Err(AppError::InvalidData(
                "remote vault identity does not match this configuration".into(),
            ));
        }
        if fence_encryption_enabled
            && !remote.released_v1_compatibility_active()
            && !protection_is_encrypted(&remote.protection)
        {
            return Err(AppError::InvalidData(
                "remote vault encryption policy would downgrade local encryption".into(),
            ));
        }
        if !remote.released_v1_compatibility_active()
            && expected_algorithm.is_some_and(|algorithm| remote.protection.algorithm != algorithm)
        {
            return Err(AppError::InvalidData(
                "remote vault encryption policy does not match this configuration".into(),
            ));
        }
        let passphrase = if protection_is_encrypted(&remote.protection) {
            match passphrase {
                VaultPassphrase::Stored => {
                    let profile = match stored_profile {
                        Some(profile) => profile,
                        None => self.active_credential_profile(settings).await?,
                    };
                    let passphrase = profile
                        .sync_passphrase()
                        .ok_or_else(|| AppError::Credential("sync passphrase is missing".into()))?;
                    Some(Zeroizing::new(passphrase.to_owned()))
                }
                VaultPassphrase::Provided(value) => Some(Zeroizing::new(
                    value
                        .ok_or_else(|| {
                            AppError::Configuration("启用包加密时必须填写同步密码".into())
                        })?
                        .to_owned(),
                )),
            }
        } else {
            None
        };
        let protector = match passphrase {
            Some(passphrase) => {
                derive_vault_protector(
                    &remote.protection,
                    &remote.identity.vault_id,
                    passphrase.as_str(),
                )
                .await?
            }
            None => None,
        };
        let active = match remote.state {
            VaultState::Active => remote,
            VaultState::Publishing { .. } => recover_head_publish(backend, &remote).await?,
            VaultState::Frozen { .. } => {
                recover_frozen_generation_from_snapshot(backend, &remote).await?
            }
        };
        Ok((active, protector))
    }

    async fn active_credential_profile(
        &self,
        settings: &CloudSyncSettings,
    ) -> Result<StoredCloudCredentialProfile> {
        load_or_migrate_credential_bundle(self.credentials.as_ref(), settings)
            .await?
            .map(|bundle| bundle.active.clone())
            .ok_or_else(|| AppError::Credential("cloud credentials are not configured".into()))
    }

    async fn reconcile_pending_credential_transition(
        &self,
        mut settings: AppSettings,
    ) -> Result<AppSettings> {
        let Some(mut bundle) =
            load_or_migrate_credential_bundle(self.credentials.as_ref(), &settings.cloud_sync)
                .await?
        else {
            return Ok(settings);
        };
        let Some(pending) = bundle.pending.clone() else {
            return Ok(settings);
        };
        if pending.target_vault_id != settings.cloud_sync.vault_id {
            return Err(AppError::Credential(
                "pending cloud credential transition targets another vault".into(),
            ));
        }

        let mut last_error = None;
        let mut resolved = None;
        for profile in [&pending.credentials, &bundle.active] {
            let backend = match backend_from_profile(&settings.cloud_sync, profile) {
                Ok(backend) => backend,
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            };
            match load_versioned_identity(backend.as_ref()).await {
                Ok(vault) => {
                    resolved = Some((backend, vault));
                    break;
                }
                Err(error) => last_error = Some(error),
            }
        }
        let (backend, mut remote) = resolved.ok_or_else(|| {
            last_error.unwrap_or_else(|| {
                AppError::Credential("cloud credential transition could not be reconciled".into())
            })
        })?;

        if remote.identity.vault_id != settings.cloud_sync.vault_id {
            return Err(AppError::InvalidData(
                "remote vault identity does not match this configuration".into(),
            ));
        }
        if let VaultState::Frozen {
            lease_expires_at_ms,
            ..
        } = &remote.state
            && current_epoch_millis()? < *lease_expires_at_ms
        {
            return Err(AppError::Configuration(
                "云端正在执行存档维护，请稍后重试".into(),
            ));
        }
        let remote_encrypted = protection_is_encrypted(&remote.protection);
        if settings.cloud_sync.encryption_enabled && !remote_encrypted {
            return Err(AppError::InvalidData(
                "remote vault encryption policy would downgrade local encryption".into(),
            ));
        }

        match remote.state.clone() {
            VaultState::Active => {}
            VaultState::Publishing { .. } => {
                if remote_encrypted {
                    let passphrase = bundle.active.sync_passphrase().ok_or_else(|| {
                        AppError::Credential("active sync passphrase is missing".into())
                    })?;
                    derive_vault_protector(
                        &remote.protection,
                        &remote.identity.vault_id,
                        passphrase,
                    )
                    .await?;
                }
                remote = recover_head_publish(backend.as_ref(), &remote).await?;
            }
            VaultState::Frozen {
                operation_id,
                lease_expires_at_ms,
                target_generation_id,
                target_protection,
                stage,
                ..
            } => {
                if current_epoch_millis()? < lease_expires_at_ms {
                    return Err(AppError::Configuration(
                        "云端正在执行存档维护，请稍后重试".into(),
                    ));
                }
                if remote_encrypted {
                    let passphrase = bundle.active.sync_passphrase().ok_or_else(|| {
                        AppError::Credential("active sync passphrase is missing".into())
                    })?;
                    derive_vault_protector(
                        &remote.protection,
                        &remote.identity.vault_id,
                        passphrase,
                    )
                    .await?;
                }
                let pending_owns_freeze = operation_id == pending.operation_id
                    && target_generation_id == pending.target_generation_id
                    && remote.identity.vault_id == pending.target_vault_id;
                if pending_owns_freeze && pending.phase == CredentialTransitionPhase::Prepared {
                    bundle.set_pending_phase(
                        &pending.operation_id,
                        CredentialTransitionPhase::RemoteFrozen,
                    )?;
                    save_credential_bundle(
                        self.credentials.as_ref(),
                        &settings.cloud_sync.remote_id,
                        &bundle,
                    )
                    .await?;
                }
                if pending_owns_freeze && stage == GenerationMaintenanceStage::ReadyToActivate {
                    if protection_is_encrypted(&target_protection) {
                        let passphrase =
                            pending.credentials.sync_passphrase().ok_or_else(|| {
                                AppError::Credential("pending sync passphrase is missing".into())
                            })?;
                        derive_vault_protector(
                            &target_protection,
                            &remote.identity.vault_id,
                            passphrase,
                        )
                        .await?;
                    }
                    remote =
                        match activate_frozen_generation_outcome(backend.as_ref(), &remote).await {
                            VaultUpdateOutcome::Committed(vault) => vault,
                            VaultUpdateOutcome::Rejected { current, .. } => current,
                            VaultUpdateOutcome::Unknown(error) => return Err(error),
                        };
                } else {
                    remote =
                        recover_frozen_generation_from_snapshot(backend.as_ref(), &remote).await?;
                }
            }
        }
        if remote.state != VaultState::Active {
            return Err(AppError::Configuration(
                "云端正在执行存档维护，请稍后重试".into(),
            ));
        }
        if remote.identity.vault_id == pending.target_vault_id
            && remote.identity.generation_id == pending.target_generation_id
        {
            if protection_is_encrypted(&remote.protection) {
                let passphrase = pending.credentials.sync_passphrase().ok_or_else(|| {
                    AppError::Credential("pending sync passphrase is missing".into())
                })?;
                derive_vault_protector(&remote.protection, &remote.identity.vault_id, passphrase)
                    .await?;
            }
            bundle.set_pending_phase(
                &pending.operation_id,
                CredentialTransitionPhase::RemoteCommitted,
            )?;
            save_credential_bundle(
                self.credentials.as_ref(),
                &settings.cloud_sync.remote_id,
                &bundle,
            )
            .await?;
            bundle.promote_pending(&pending.operation_id)?;
            save_credential_bundle(
                self.credentials.as_ref(),
                &settings.cloud_sync.remote_id,
                &bundle,
            )
            .await?;
            settings.cloud_sync.generation_id = remote.identity.generation_id;
            settings.cloud_sync.encryption_enabled = protection_is_encrypted(&remote.protection);
            return self.settings.update(settings).await;
        }
        if remote.identity.vault_id == settings.cloud_sync.vault_id
            && remote.identity.generation_id == settings.cloud_sync.generation_id
        {
            bundle.discard_pending(&pending.operation_id)?;
            save_credential_bundle(
                self.credentials.as_ref(),
                &settings.cloud_sync.remote_id,
                &bundle,
            )
            .await?;
            return Ok(settings);
        }
        Err(AppError::InvalidData(
            "remote vault state does not match the pending credential transition".into(),
        ))
    }

    async fn remote_devices(
        &self,
        backend: &dyn CloudBackend,
        settings: &CloudSyncSettings,
        local: &DeviceState,
    ) -> Result<Vec<RemoteDeviceStatus>> {
        let devices_path = RemotePath::parse(&format!(
            "v1/generations/{}/devices",
            settings.generation_id
        ))
        .map_err(|error| AppError::InvalidData(error.to_string()))?;
        let entries = match backend.list_depth_one(&devices_path).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == "not_found" => Vec::new(),
            Err(error) => return Err(map_cloud_error(error)),
        };
        let mut devices = Vec::new();
        for entry in entries.into_iter().filter(|entry| {
            entry.is_collection && entry.name != "baseline" && entry.name != local.device_id
        }) {
            let head_path = devices_path
                .join(&entry.name)
                .and_then(|path| path.join("head.json"))
                .map_err(|error| AppError::InvalidData(error.to_string()))?;
            let object = match backend.get(&head_path).await {
                Ok(object) => object,
                Err(error) if error.kind() == "not_found" => continue,
                Err(error) => return Err(map_cloud_error(error)),
            };
            let head: HeadDocument = serde_json::from_slice(&object.bytes)?;
            if head.generation_id != settings.generation_id || head.device_id != entry.name {
                return Err(AppError::InvalidData(
                    "remote device head identity is invalid".into(),
                ));
            }
            let last_seen_at = self
                .sync_store
                .remote_cursor(&settings.generation_id, &entry.name)
                .await?
                .and_then(|cursor| chrono::DateTime::from_timestamp_millis(cursor.updated_at_ms))
                .map(|value| value.to_rfc3339());
            devices.push(RemoteDeviceStatus {
                display_name: entry.name.clone(),
                device_id: entry.name,
                last_seen_at,
            });
        }
        devices.sort_by(|left, right| left.device_id.cmp(&right.device_id));
        Ok(devices)
    }

    async fn mark_cloud_syncing(&self) {
        self.cloud_sync_runtime.write().await.state = CloudSyncState::Syncing;
    }

    async fn mark_cloud_success(&self, devices: Vec<RemoteDeviceStatus>) {
        let mut runtime = self.cloud_sync_runtime.write().await;
        runtime.state = CloudSyncState::Idle;
        runtime.last_success_at = Some(chrono::Utc::now().to_rfc3339());
        runtime.last_error_code = None;
        runtime.last_error_message = None;
        runtime.devices = devices;
    }

    async fn mark_cloud_error(&self, error: &AppError) {
        let (state, code) = classify_cloud_error(error);
        let mut runtime = self.cloud_sync_runtime.write().await;
        runtime.state = state;
        runtime.last_error_code = Some(code.into());
        runtime.last_error_message = Some(error.to_string());
    }
    pub async fn rotate_secret(&self) -> Result<AppSettings> {
        self.settings.rotate_secret().await
    }

    /// Rejects writes once the service is shutting down. Call this at the top
    /// of every write path, before acquiring the sync_gate, so a successful
    /// `move_data_directory` blocks subsequent writes immediately rather than
    /// queuing behind the gate or mutating the old pool.
    fn ensure_writable(&self) -> Result<()> {
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(AppError::Cancelled(
                "应用正在重启以切换数据目录，请稍后重试".into(),
            ));
        }
        Ok(())
    }

    pub async fn move_data_directory(&self, directory: &Path) -> Result<()> {
        self.ensure_writable()?;
        // The destination probe and the VACUUM snapshot must run inside
        // the same sync_gate critical section. Checking the destination before
        // taking the gate lets two concurrent moves both pass the probe and
        // makes the loser fail inside VACUUM with an opaque SQLite error.
        let _guard = self.sync_gate.lock().await;
        tokio::fs::create_dir_all(directory).await?;
        let destination = directory.join("chat_memory.db");
        if destination.exists() {
            return Err(AppError::Configuration(
                "目标目录中已存在 chat_memory.db，请选择其他目录".into(),
            ));
        }
        sqlx::query("VACUUM INTO ?")
            .bind(destination.to_string_lossy().as_ref())
            .execute(&self.pool)
            .await?;
        let mut settings = self.settings().await;
        settings.data_directory = Some(directory.to_string_lossy().into_owned());
        self.settings.update(settings).await?;
        // The snapshot is complete and a restart has been requested. Set the
        // shutdown flag before releasing the sync_gate so every subsequent
        // write path observes it and rejects new work, leaving the old pool
        // untouched until the process actually restarts.
        self.shutdown.store(true, Ordering::SeqCst);
        tracing::info!(destination=%directory.display(), "database copied to configured directory; service is now draining pending restart");
        Ok(())
    }

    pub async fn set_close_behavior(&self, behavior: CloseBehavior) -> Result<()> {
        let mut settings = self.settings().await;
        settings.close_behavior = behavior;
        // close_behavior is a pure local UI preference: persist it without taking
        // sync_gate or running cloud credential reconciliation, so saving
        // it is never blocked by a slow/unreachable cloud backend.
        self.update_local_settings(settings).await?;
        Ok(())
    }

    /// Persists pure-local settings (close behavior, tray click, theme, language)
    /// without acquiring `sync_gate` and without cloud credential reconciliation
    /// or remote validation. A local mutation is still queued so the
    /// change propagates via the normal sync path on the next run, but saving the
    /// preference itself only touches the local SettingsStore and is therefore
    /// never blocked by cloud connectivity.
    pub async fn update_local_settings(&self, mut settings: AppSettings) -> Result<AppSettings> {
        // Preserve cloud-sync-affecting fields exactly as the committed copy held
        // them: this path must not silently change cloud_sync/secret state, which
        // would bypass the gated credential transition. Only local UI preferences
        // are allowed to change.
        let committed = self.settings.get().await;
        settings.cloud_sync = committed.cloud_sync;
        settings.secret = committed.secret.clone();
        settings.secret_enabled = committed.secret_enabled;
        settings.allowed_origins = committed.allowed_origins;
        settings.semantic_search = committed.semantic_search;
        let updated = self.settings.update(settings).await?;
        self.notify_local_sync();
        Ok(updated)
    }

    pub async fn import(&self, request: ImportRequest) -> Result<ImportResponse> {
        self.ensure_writable()?;
        let platform = request.platform.clone();
        let received = request.sessions.len();
        let normalized = request
            .sessions
            .iter()
            .map(|raw| normalizer::normalize_session(&request.platform, raw))
            .collect::<Result<Vec<_>>>()?;
        let imported = {
            let _guard = self.sync_gate.lock().await;
            import_local_sessions(&self.pool, &normalized).await?
        };
        for session in &normalized {
            if let Ok(Some(id)) = sqlx::query_scalar::<_, String>(
                "SELECT id FROM sessions WHERE platform = ? AND platform_session_id = ?",
            )
            .bind(&session.platform)
            .bind(&session.platform_session_id)
            .fetch_optional(&self.pool)
            .await
            {
                let _ = self.semantic.request_session_index(&id).await;
            }
        }
        tracing::info!(%platform, received, imported, "session import completed");
        self.notify_local_sync();
        Ok(ImportResponse {
            imported,
            skipped: 0,
        })
    }

    /// 多格式历史导入统一入口：按内容嗅探 DeepSeek ZIP / Cherry Studio /
    /// Chatbox / Kelivo / Gemini Takeout，解析后走同一落库路径。
    pub async fn import_history(&self, bytes: Vec<u8>) -> Result<ImportResponse> {
        self.ensure_writable()?;
        let archive_bytes = bytes.len();
        if archive_bytes > MAX_HISTORY_ARCHIVE_BYTES {
            return Err(AppError::InvalidData("导入文件超过 128 MB 限制".into()));
        }
        let archive = import_history::parse_import_history(bytes).await?;
        let normalized = archive.sessions;
        let imported = {
            let _guard = self.sync_gate.lock().await;
            import_local_sessions(&self.pool, &normalized).await?
        };
        for session in &normalized {
            if let Ok(Some(id)) = sqlx::query_scalar::<_, String>(
                "SELECT id FROM sessions WHERE platform = ? AND platform_session_id = ?",
            )
            .bind(&session.platform)
            .bind(&session.platform_session_id)
            .fetch_optional(&self.pool)
            .await
            {
                let _ = self.semantic.request_session_index(&id).await;
            }
        }
        tracing::info!(
            format = archive.format,
            archive_bytes,
            sessions = normalized.len(),
            imported,
            "history archive import completed"
        );
        self.notify_local_sync();
        Ok(ImportResponse {
            imported,
            skipped: 0,
        })
    }

    pub async fn list(&self, query: SearchQuery) -> Result<SessionList> {
        self.semantic.search_sessions(query).await
    }

    pub async fn open_session(&self, id: &str, anchor_seq: Option<i64>) -> Result<SessionOpen> {
        database::open_session(&self.pool, id, anchor_seq).await
    }

    pub async fn session_messages(
        &self,
        id: &str,
        start_seq: i64,
        limit: i64,
    ) -> Result<Vec<Message>> {
        database::get_session_messages(&self.pool, id, start_seq, limit).await
    }

    pub async fn session_search_hits(
        &self,
        id: &str,
        query: &str,
        mode: Option<SearchMode>,
    ) -> Result<Vec<SessionSearchHit>> {
        let settings = self.settings().await;
        let mode = mode.unwrap_or(settings.semantic_search.default_mode);
        self.semantic.search_session_hits(id, query, mode).await
    }

    pub async fn session_branches(&self, id: &str) -> Result<BranchOverview> {
        database::get_session_branches(&self.pool, id).await
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        self.ensure_writable()?;
        {
            let _guard = self.sync_gate.lock().await;
            delete_local_session(&self.pool, id).await?;
        }
        let _ = self.semantic.delete_session(id).await;
        tracing::info!("session deleted");
        self.notify_local_sync();
        Ok(())
    }

    pub async fn sync_status(&self, platform: &str) -> Result<Option<String>> {
        database::sync_status(&self.pool, platform).await
    }

    pub async fn api_status(&self) -> ApiStatus {
        self.api_status.read().await.clone()
    }

    pub async fn set_api_status(&self, status: ApiStatus) {
        *self.api_status.write().await = status;
    }

    pub async fn mark_userscript_request(&self) {
        *self.last_userscript_request_at.write().await = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|value| value.as_secs());
    }

    pub async fn desktop_api_status(&self) -> DesktopApiStatus {
        let last = *self.last_userscript_request_at.read().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|value| value.as_secs());
        DesktopApiStatus {
            service: self.api_status().await,
            userscript_connected: last
                .zip(now)
                .is_some_and(|(last, now)| now.saturating_sub(last) <= 15),
            last_userscript_request_at: last,
            mcp: crate::local_services::LocalServiceStatus::Stopped,
            mcp_url: crate::mcp::server::MCP_URL.to_string(),
        }
    }

    pub async fn semantic_status(&self) -> SemanticRuntimeStatus {
        self.semantic.runtime_status().await
    }

    pub async fn embedding_healthcheck(&self) -> EmbeddingHealth {
        self.semantic.healthcheck().await
    }

    pub async fn reindex_semantic_with_progress(
        &self,
        on_progress: Option<std::sync::Arc<dyn Fn(crate::models::ReindexProgress) + Send + Sync>>,
    ) -> Result<usize> {
        self.semantic
            .request_reindex_all_with_progress(on_progress)
            .await
    }

    pub async fn download_local_model(
        &self,
        on_progress: Option<crate::embedding::local::DownloadProgressCallback>,
    ) -> Result<()> {
        self.semantic.ensure_local_model(on_progress).await
    }

    pub async fn import_local_model(&self, path: &Path) -> Result<()> {
        self.semantic.import_local_model(path).await
    }

    pub async fn cancel_semantic_work(&self) -> Result<()> {
        self.semantic.cancel_semantic_work().await
    }
}

fn map_cloud_error(error: CloudError) -> AppError {
    AppError::Cloud(error)
}

fn classify_cloud_error(error: &AppError) -> (CloudSyncState, &'static str) {
    match error {
        AppError::Cloud(error) => match error.kind() {
            "auth" => (CloudSyncState::AuthError, "auth"),
            "offline" => (CloudSyncState::Offline, "offline"),
            "precondition" => (CloudSyncState::ProtocolError, "precondition"),
            "not_found" => (CloudSyncState::ProtocolError, "not_found"),
            _ => (CloudSyncState::ProtocolError, "protocol"),
        },
        AppError::Credential(_) | AppError::Crypto(_) => {
            (CloudSyncState::NeedsUnlock, "needs_unlock")
        }
        _ => (CloudSyncState::ProtocolError, "protocol"),
    }
}

fn validate_encryption_credentials(
    settings: &CloudSyncSettings,
    credentials: &CloudCredentialInput,
) -> Result<()> {
    let sync_password = match credentials {
        CloudCredentialInput::Webdav { sync_password, .. }
        | CloudCredentialInput::S3 { sync_password, .. } => sync_password.as_deref(),
    };
    if settings.encryption_enabled && sync_password.is_none_or(str::is_empty) {
        return Err(AppError::Configuration(
            "启用包加密时必须填写同步密码".into(),
        ));
    }
    Ok(())
}

fn prepare_cloud_sync_transition(
    previous: &CloudSyncSettings,
    next: &mut CloudSyncSettings,
) -> bool {
    next.normalize();
    if same_cloud_connection(previous, next) {
        return false;
    }
    next.connection_verified = false;
    if next.remote_id.is_empty() || next.remote_id == previous.remote_id {
        next.rotate_remote_identity();
    }
    true
}

fn same_cloud_connection(previous: &CloudSyncSettings, next: &CloudSyncSettings) -> bool {
    let mut previous = previous.clone();
    let mut next = next.clone();
    previous.normalize();
    next.normalize();
    if previous.backend != next.backend {
        return false;
    }
    match previous.backend {
        CloudBackendKind::Webdav => {
            previous.base_url.trim() == next.base_url.trim()
                && previous.root_path.trim().trim_matches('/')
                    == next.root_path.trim().trim_matches('/')
                && previous.username.trim() == next.username.trim()
        }
        CloudBackendKind::S3 => previous.s3 == next.s3,
    }
}

fn validate_cloud_sync_update(
    previous: &CloudSyncSettings,
    next: &mut CloudSyncSettings,
    connection_tested: bool,
) -> Result<()> {
    next.normalize();
    let connection_changed = !same_cloud_connection(previous, next);
    let legacy_enabled_webdav = previous.enabled
        && previous.backend == CloudBackendKind::Webdav
        && !previous.connection_verified;
    next.connection_verified = connection_tested
        || (!connection_changed && (previous.connection_verified || legacy_enabled_webdav));
    if next.enabled && !next.connection_verified {
        return Err(AppError::Configuration("云同步连接尚未验证".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
