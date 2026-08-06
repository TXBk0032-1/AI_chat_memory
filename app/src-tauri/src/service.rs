use serde_json::Value;
use sqlx::SqlitePool;
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
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

const MAX_CONVERSATIONS_JSON_BYTES: u64 = 512 * 1024 * 1024;
const CONVERSATIONS_JSON_TOO_LARGE: &str = "conversations.json 解压后超过 512 MB 限制";

fn read_zip_entry_with_limit<R: Read>(reader: R, max_bytes: u64) -> Result<String> {
    let mut content = String::new();
    let mut limited = reader.take(max_bytes.saturating_add(1));
    limited.read_to_string(&mut content)?;
    if content.len() as u64 > max_bytes {
        return Err(AppError::InvalidData(CONVERSATIONS_JSON_TOO_LARGE.into()));
    }
    Ok(content)
}

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

#[derive(Clone)]
pub struct AppService {
    pool: SqlitePool,
    settings: Arc<SettingsStore>,
    semantic: Arc<SemanticEngine>,
    api_status: Arc<RwLock<ApiStatus>>,
    last_userscript_request_at: Arc<RwLock<Option<u64>>>,
    sync_store: SyncStore,
    credentials: Arc<dyn CredentialStore>,
    cloud_sync_scheduler: CloudSyncScheduler,
    sync_gate: Arc<Mutex<()>>,
    cloud_sync_runtime: Arc<RwLock<CloudSyncRuntime>>,
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
        semantic.start_worker();
        let sync_store = SyncStore::new(pool.clone());
        let credentials: Arc<dyn CredentialStore> =
            Arc::new(SystemCredentialStore::new("ai-chat-memory"));
        let (cloud_sync_scheduler, worker_receiver) = CloudSyncScheduler::production();
        let service = Self {
            pool,
            settings,
            semantic,
            api_status: Arc::new(RwLock::new(ApiStatus::Starting)),
            last_userscript_request_at: Arc::new(RwLock::new(None)),
            sync_store,
            credentials,
            cloud_sync_scheduler,
            sync_gate: Arc::new(Mutex::new(())),
            cloud_sync_runtime: Arc::new(RwLock::new(CloudSyncRuntime::default())),
        };
        service.start_cloud_sync_worker(worker_receiver);
        Ok(service)
    }

    fn start_cloud_sync_worker(&self, mut receiver: mpsc::Receiver<CloudSyncCommand>) {
        let service = self.clone();
        tokio::spawn(async move {
            let mut worker = CloudSyncWorkerState::default();
            worker.submit(SyncTrigger::Startup, Instant::now());
            let mut manual_waiters = Vec::new();
            loop {
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

    pub async fn settings(&self) -> AppSettings {
        self.settings.get().await
    }

    pub async fn update_settings(&self, settings: AppSettings) -> Result<AppSettings> {
        self.update_settings_with_cloud_credentials(settings, None)
            .await
    }

    pub async fn update_settings_with_cloud_credentials(
        &self,
        mut settings: AppSettings,
        credentials: Option<CloudCredentialInput>,
    ) -> Result<AppSettings> {
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
            );
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
        let _guard = self.sync_gate.lock().await;
        let settings = self.settings().await;
        if !settings.cloud_sync.enabled {
            self.cloud_sync_runtime.write().await.state = CloudSyncState::Disabled;
            return Ok(self.cloud_sync_status().await);
        }
        self.mark_cloud_syncing().await;
        match self.sync_once_locked(settings).await {
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
        );
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
        );
        if let Err(error) = old_engine.rewrite_generation(&new_generation).await {
            self.cleanup_unactivated_generation(backend.as_ref(), &old_generation, &new_generation)
                .await;
            return Err(error);
        }
        settings.cloud_sync.generation_id = new_generation.clone();
        settings.cloud_sync.encryption_enabled = remote_encryption;
        self.settings.update(settings.clone()).await?;

        let new_engine = SyncEngine::new_protected_with_policy(
            self.sync_store.clone(),
            backend.clone(),
            &current.identity.vault_id,
            &new_generation,
            device.device_id.clone(),
            current.protection,
            protector,
        );
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
        self.sync_store
            .remove_remote_cursor(&settings.cloud_sync.generation_id, &device_id)
            .await?;
        let devices = self
            .remote_devices(backend.as_ref(), &settings.cloud_sync, &local)
            .await?;
        self.mark_cloud_success(devices).await;
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

    pub async fn move_data_directory(&self, directory: &Path) -> Result<()> {
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
        self.update_settings(settings).await?;
        tracing::info!(destination=%directory.display(), "database copied to configured directory; restarting application");
        Ok(())
    }

    pub async fn set_close_behavior(&self, behavior: CloseBehavior) -> Result<()> {
        let mut settings = self.settings().await;
        settings.close_behavior = behavior;
        self.update_settings(settings).await?;
        Ok(())
    }

    pub async fn import(&self, request: ImportRequest) -> Result<ImportResponse> {
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

    pub async fn import_deepseek_zip(&self, bytes: Vec<u8>) -> Result<ImportResponse> {
        let archive_bytes = bytes.len();
        if bytes.len() > 128 * 1024 * 1024 {
            return Err(AppError::InvalidData("ZIP 文件超过 128 MB 限制".into()));
        }
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
        let file = archive
            .by_name("conversations.json")
            .map_err(|_| AppError::InvalidData("ZIP 中缺少 conversations.json".into()))?;
        if file.size() > MAX_CONVERSATIONS_JSON_BYTES {
            return Err(AppError::InvalidData(CONVERSATIONS_JSON_TOO_LARGE.into()));
        }
        if file.compressed_size() > 0 && file.size() / file.compressed_size() > 200 {
            return Err(AppError::InvalidData("ZIP 压缩比异常".into()));
        }
        let content = read_zip_entry_with_limit(file, MAX_CONVERSATIONS_JSON_BYTES)?;
        let conversations: Vec<Value> = serde_json::from_str(&content)?;
        let normalized = conversations
            .iter()
            .map(normalizer::normalize_deepseek_export)
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
        tracing::info!(
            archive_bytes,
            conversations = normalized.len(),
            imported,
            "DeepSeek archive import completed"
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
mod cloud_backend_transition_tests {
    use super::{
        AppService, CloudSyncCommand, CloudSyncRuntime, CloudSyncScheduler, CloudSyncWorkerState,
        VaultPassphrase, VaultVerification, classify_cloud_error, import_local_sessions,
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
                api_status: Arc::new(RwLock::new(ApiStatus::Starting)),
                last_userscript_request_at: Arc::new(RwLock::new(None)),
                sync_store: SyncStore::new(pool),
                credentials: Arc::new(MemoryCredentialStore::default()),
                cloud_sync_scheduler: CloudSyncScheduler::for_tests(),
                sync_gate: Arc::new(Mutex::new(())),
                cloud_sync_runtime: Arc::new(RwLock::new(CloudSyncRuntime::default())),
            },
            data_dir,
        )
    }

    async fn service_with_local_session() -> AppService {
        service_with_local_session_fixture().await.0
    }

    async fn publish_released_plain_fixture(
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
        let bundle =
            load_credential_bundle(service.credentials.as_ref(), "remote-settings-failure")
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
        let switched_bundle =
            load_credential_bundle(service.credentials.as_ref(), &draft_remote_id)
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
        assert!(operations.contains(&(
            "fixture".into(),
            "live-2".into(),
            MutationOperation::Upsert,
        )));
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
            api_status: Arc::new(RwLock::new(ApiStatus::Starting)),
            last_userscript_request_at: Arc::new(RwLock::new(None)),
            sync_store: SyncStore::new(service.pool.clone()),
            credentials: Arc::new(memory.clone()),
            cloud_sync_scheduler: CloudSyncScheduler::for_tests(),
            sync_gate: Arc::new(Mutex::new(())),
            cloud_sync_runtime: Arc::new(RwLock::new(CloudSyncRuntime::default())),
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
        let reconciled = crate::sync::credentials::load_credential_bundle(
            &memory,
            &persisted.cloud_sync.remote_id,
        )
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
            VaultProtection::encrypted(&settings.cloud_sync.vault_id, "correct-passphrase")
                .unwrap(),
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
            VaultProtection::encrypted(&settings.cloud_sync.vault_id, "correct-passphrase")
                .unwrap(),
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
            configured_s3_service_for_sync_guard_tests("plain-fence", true, "local-passphrase")
                .await;
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
            VaultProtection::encrypted(&settings.cloud_sync.vault_id, "correct-passphrase")
                .unwrap();
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
            VaultProtection::encrypted(&settings.cloud_sync.vault_id, "correct-passphrase")
                .unwrap();
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
            VaultProtection::encrypted(&settings.cloud_sync.vault_id, "correct-passphrase")
                .unwrap(),
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
            VaultProtection::encrypted(&settings.cloud_sync.vault_id, "correct-passphrase")
                .unwrap(),
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
            configured_s3_service_for_sync_guard_tests(
                "settings-plain-fence",
                true,
                "old-passphrase",
            )
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
            configured_s3_service_for_sync_guard_tests(
                "rewrite-plain-fence",
                true,
                "old-passphrase",
            )
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
            configured_s3_service_for_sync_guard_tests(
                "enable-encryption",
                false,
                "old-passphrase",
            )
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
    async fn settings_write_failure_after_rotation_keeps_the_active_generation_and_new_credentials()
    {
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
                service.update_settings(next).await
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
            sqlx::query_scalar::<_, i64>(
                "SELECT next_seq FROM sync_device_state WHERE singleton = 1"
            )
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
            crate::sync::backend::RemotePath::parse("v1/generations/generation-old/keep.bin")
                .unwrap();
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
            crate::sync::backend::RemotePath::parse("v1/generations/generation-old/keep.bin")
                .unwrap();
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
}

#[cfg(test)]
mod zip_import_tests {
    use super::{CONVERSATIONS_JSON_TOO_LARGE, read_zip_entry_with_limit};
    use crate::error::AppError;
    use std::io::{Cursor, Write};
    use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

    #[test]
    fn rejects_zip_entry_that_exceeds_actual_output_limit_with_forged_metadata() {
        let mut archive_bytes = Vec::new();
        {
            let mut writer = ZipWriter::new(Cursor::new(&mut archive_bytes));
            writer
                .start_file(
                    "conversations.json",
                    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
                )
                .unwrap();
            writer.write_all(&vec![b'a'; 2 * 1024]).unwrap();
            writer.finish().unwrap();
        }

        let central_header = archive_bytes
            .windows(4)
            .rposition(|window| window == b"PK\x01\x02")
            .unwrap();
        archive_bytes[central_header + 24..central_header + 28]
            .copy_from_slice(&1_u32.to_le_bytes());

        let mut archive = ZipArchive::new(Cursor::new(archive_bytes)).unwrap();
        let file = archive.by_name("conversations.json").unwrap();
        assert_eq!(file.size(), 1, "测试 ZIP 必须伪造较小的声明大小");

        assert!(matches!(
            read_zip_entry_with_limit(file, 1024),
            Err(AppError::InvalidData(message)) if message == CONVERSATIONS_JSON_TOO_LARGE
        ));
    }

    #[test]
    fn accepts_zip_entry_at_actual_output_limit() {
        let content = read_zip_entry_with_limit(Cursor::new(b"[]"), 2).unwrap();
        assert_eq!(content, "[]");
    }
}

#[cfg(test)]
mod cloud_encryption_tests {
    use super::validate_encryption_credentials;
    use crate::models::{CloudCredentialInput, CloudSyncSettings};
    use crate::sync::vault::VaultProtection;

    #[test]
    fn sync_protector_is_stable_within_a_vault_and_separated_between_vaults() {
        let first_policy = VaultProtection::encrypted("vault-a", "shared password").unwrap();
        let other_policy = VaultProtection::encrypted("vault-b", "shared password").unwrap();
        let first = first_policy
            .derive_protector("vault-a", "shared password")
            .unwrap()
            .unwrap();
        let second = first_policy
            .derive_protector("vault-a", "shared password")
            .unwrap()
            .unwrap();
        let other_vault = other_policy
            .derive_protector("vault-b", "shared password")
            .unwrap()
            .unwrap();
        let nonce = [7_u8; 24];
        let ciphertext = first.seal(b"header", b"payload", nonce).unwrap();

        assert_eq!(
            second.open(b"header", &ciphertext, nonce).unwrap(),
            b"payload"
        );
        assert!(other_vault.open(b"header", &ciphertext, nonce).is_err());
    }

    #[test]
    fn encrypted_connection_requires_a_sync_passphrase() {
        let settings = CloudSyncSettings {
            encryption_enabled: true,
            ..CloudSyncSettings::default()
        };
        let missing = CloudCredentialInput::Webdav {
            password: "webdav".into(),
            sync_password: None,
        };
        let present = CloudCredentialInput::Webdav {
            password: "webdav".into(),
            sync_password: Some("shared".into()),
        };

        assert!(validate_encryption_credentials(&settings, &missing).is_err());
        assert!(validate_encryption_credentials(&settings, &present).is_ok());
    }
}
