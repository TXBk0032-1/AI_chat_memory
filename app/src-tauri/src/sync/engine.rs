use crate::{
    error::{AppError, Result},
    sync::{
        backend::{CloudBackend, RemotePath},
        bundle::{
            BundleLimits, DecodedBundle, ProtectionAlgorithm, SealedBundle, is_bundle_limit_error,
            open_bundle_protected, open_released_v1_unchained_bundle_protected,
            seal_bundle_protected_with_limits, seal_bundle_with_limits,
        },
        crypto::PayloadProtector,
        merge::MergeEngine,
        store::{PendingMutation, RemoteObjectAnchor, SyncStore, current_time_millis},
        types::{BundleChange, BundleContents, SyncTrigger},
        vault::{
            DEFAULT_MAINTENANCE_LEASE_MS, HeadPublishRequest, VaultCompatibility, VaultProtection,
            VaultState, VaultUpdateOutcome, VersionedVaultIdentity,
            activate_frozen_generation_outcome, begin_generation_freeze_owned_with_policy,
            begin_head_publish, load_versioned_identity, mark_frozen_generation_ready,
            recover_head_publish, rollback_frozen_generation,
        },
    },
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::Mutex;

const MAX_MUTATIONS_PER_BUNDLE: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeadDocument {
    pub generation_id: String,
    pub device_id: String,
    pub end_seq: i64,
    pub path: String,
    pub sha256: String,
}

struct RemoteBundleDownload {
    decoded: DecodedBundle,
    released_v1_unchained: bool,
    path: String,
    sha256: String,
}

struct ReleasedV1BundleCandidate {
    path: String,
    sha256: String,
    bundle: RemoteBundleDownload,
}

struct ReleasedV1Reconstruction<'a> {
    devices_path: &'a RemotePath,
    remote_device_id: &'a str,
    legacy_head: &'a HeadDocument,
    cursor: i64,
    cursor_anchor: Option<&'a RemoteObjectAnchor>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub pulled: usize,
    pub published: usize,
    pub acknowledged: usize,
}

#[derive(Debug)]
pub enum RotationOutcome {
    Committed {
        operation_id: String,
        report: SyncReport,
        vault: VersionedVaultIdentity,
    },
    RolledBack {
        operation_id: String,
        vault: VersionedVaultIdentity,
        error: AppError,
    },
    Unknown {
        operation_id: String,
        error: AppError,
    },
}

impl RotationOutcome {
    pub fn operation_id(&self) -> &str {
        match self {
            Self::Committed { operation_id, .. }
            | Self::RolledBack { operation_id, .. }
            | Self::Unknown { operation_id, .. } => operation_id,
        }
    }

    pub fn into_result(self) -> Result<SyncReport> {
        match self {
            Self::Committed { report, .. } => Ok(report),
            Self::RolledBack { error, .. } | Self::Unknown { error, .. } => Err(error),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PullPolicy {
    TolerantSync,
    StrictMaintenance,
}

#[derive(Debug, Clone)]
pub struct SchedulerState {
    pending: Option<SyncTrigger>,
    pub paused_for_auth: bool,
    pub retry_delay: Duration,
}

impl Default for SchedulerState {
    fn default() -> Self {
        Self {
            pending: None,
            paused_for_auth: false,
            retry_delay: Duration::from_secs(30),
        }
    }
}

impl SchedulerState {
    pub fn submit(&mut self, trigger: SyncTrigger) {
        if trigger == SyncTrigger::Manual {
            self.paused_for_auth = false;
        }
        if self
            .pending
            .as_ref()
            .is_none_or(|current| priority(trigger) > priority(*current))
        {
            self.pending = Some(trigger);
        }
    }

    pub fn take(&mut self) -> Option<SyncTrigger> {
        self.pending.take()
    }

    pub fn delay_for(trigger: SyncTrigger) -> Duration {
        match trigger {
            SyncTrigger::Manual => Duration::ZERO,
            SyncTrigger::LocalMutation => Duration::from_secs(5),
            SyncTrigger::Startup => Duration::from_secs(30),
            SyncTrigger::Periodic => Duration::from_secs(15 * 60),
        }
    }

    pub fn success(&mut self) {
        self.retry_delay = Duration::from_secs(30);
    }

    pub fn failure(&mut self, authentication: bool) {
        self.paused_for_auth = authentication;
        self.retry_delay = (self.retry_delay * 2).min(Duration::from_secs(60 * 60));
    }

    pub fn retry_delay_with_jitter(&self, entropy: u32) -> Duration {
        let window = self.retry_delay.as_millis() / 5;
        let offset = (u128::from(entropy) % (window.saturating_mul(2).saturating_add(1))) as i128
            - window as i128;
        let base = self.retry_delay.as_millis() as i128;
        Duration::from_millis(base.saturating_add(offset).max(0) as u64)
    }
}

fn priority(trigger: SyncTrigger) -> u8 {
    match trigger {
        SyncTrigger::Periodic => 0,
        SyncTrigger::Startup => 1,
        SyncTrigger::LocalMutation => 2,
        SyncTrigger::Manual => 3,
    }
}

pub struct SyncEngine<B: ?Sized> {
    store: SyncStore,
    backend: Arc<B>,
    vault_id: String,
    generation_id: String,
    device_id: String,
    vault_protection: Option<VaultProtection>,
    protector: Option<Arc<dyn PayloadProtector>>,
    bundle_limits: BundleLimits,
    single_flight: Mutex<()>,
}

impl<B: CloudBackend + ?Sized + 'static> SyncEngine<B> {
    pub fn new(
        store: SyncStore,
        backend: Arc<B>,
        vault_id: impl Into<String>,
        generation_id: impl Into<String>,
        device_id: impl Into<String>,
    ) -> Self {
        Self::build(
            store,
            backend,
            vault_id,
            generation_id,
            device_id,
            Some(VaultProtection::plain()),
            None,
        )
    }

    pub fn new_protected(
        store: SyncStore,
        backend: Arc<B>,
        vault_id: impl Into<String>,
        generation_id: impl Into<String>,
        device_id: impl Into<String>,
        protector: Option<Arc<dyn PayloadProtector>>,
    ) -> Self {
        Self::build(
            store,
            backend,
            vault_id,
            generation_id,
            device_id,
            None,
            protector,
        )
    }

    pub fn new_protected_with_policy(
        store: SyncStore,
        backend: Arc<B>,
        vault_id: impl Into<String>,
        generation_id: impl Into<String>,
        device_id: impl Into<String>,
        vault_protection: VaultProtection,
        protector: Option<Arc<dyn PayloadProtector>>,
    ) -> Self {
        Self::build(
            store,
            backend,
            vault_id,
            generation_id,
            device_id,
            Some(vault_protection),
            protector,
        )
    }

    fn build(
        store: SyncStore,
        backend: Arc<B>,
        vault_id: impl Into<String>,
        generation_id: impl Into<String>,
        device_id: impl Into<String>,
        vault_protection: Option<VaultProtection>,
        protector: Option<Arc<dyn PayloadProtector>>,
    ) -> Self {
        Self {
            store,
            backend,
            vault_id: vault_id.into(),
            generation_id: generation_id.into(),
            device_id: device_id.into(),
            vault_protection,
            protector,
            bundle_limits: BundleLimits::default(),
            single_flight: Mutex::new(()),
        }
    }

    #[cfg(test)]
    fn with_bundle_limits(mut self, bundle_limits: BundleLimits) -> Self {
        self.bundle_limits = bundle_limits;
        self
    }

    pub async fn run_once(&self, _trigger: SyncTrigger) -> Result<SyncReport> {
        let _guard = self.single_flight.lock().await;
        let (vault, recovered) = self.ensure_active_vault().await?;
        let mut report = self
            .pull_remote(PullPolicy::TolerantSync, vault.compatibility)
            .await?;
        if let Some((owner_device_id, count)) = recovered
            && owner_device_id == self.device_id
        {
            report.published += count;
        }
        loop {
            let published = self.publish_pending().await?;
            report.published += published.published;
            report.acknowledged += published.acknowledged;
            if published.published == 0 && published.acknowledged == 0 {
                break;
            }
        }
        Ok(report)
    }

    pub async fn run_once_with_generation_replay(
        &self,
        _trigger: SyncTrigger,
    ) -> Result<SyncReport> {
        let _guard = self.single_flight.lock().await;
        let (vault, recovered) = self.ensure_active_vault().await?;
        let mut report = self
            .pull_remote(PullPolicy::TolerantSync, vault.compatibility)
            .await?;
        if let Some((owner_device_id, count)) = recovered
            && owner_device_id == self.device_id
        {
            report.published += count;
        }
        self.store
            .replay_local_baseline_for_generation(&self.vault_id, &self.generation_id)
            .await?;
        loop {
            let published = self.publish_pending().await?;
            report.published += published.published;
            report.acknowledged += published.acknowledged;
            if published.published == 0 && published.acknowledged == 0 {
                break;
            }
        }
        Ok(report)
    }

    async fn pull_remote(
        &self,
        policy: PullPolicy,
        compatibility: Option<VaultCompatibility>,
    ) -> Result<SyncReport> {
        let devices_path =
            RemotePath::parse(&format!("v1/generations/{}/devices", self.generation_id))
                .map_err(|e| AppError::InvalidData(e.to_string()))?;
        let entries = match self.backend.list_depth_one(&devices_path).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == "not_found" => return Ok(SyncReport::default()),
            Err(error) => return Err(cloud_error(error)),
        };
        let merger = MergeEngine::new(self.store.pool().clone(), None);
        let mut report = SyncReport::default();
        for entry in entries.into_iter().filter(|entry| {
            entry.is_collection
                && (policy == PullPolicy::StrictMaintenance || entry.name != self.device_id)
        }) {
            match self
                .pull_remote_device(&devices_path, &entry.name, &merger, compatibility)
                .await
            {
                Ok(pulled) => report.pulled += pulled,
                Err(error)
                    if policy == PullPolicy::TolerantSync && is_remote_data_error(&error) =>
                {
                    tracing::warn!(
                        device_id = %entry.name,
                        %error,
                        "remote sync source was skipped"
                    );
                }
                Err(error) => return Err(error),
            }
        }
        Ok(report)
    }

    async fn pull_remote_device(
        &self,
        devices_path: &RemotePath,
        remote_device_id: &str,
        merger: &MergeEngine,
        compatibility: Option<VaultCompatibility>,
    ) -> Result<usize> {
        let head_path = devices_path
            .join(remote_device_id)
            .and_then(|path| path.join("head.json"))
            .map_err(|error| AppError::InvalidData(error.to_string()))?;
        let head_object = self
            .backend
            .get(&head_path)
            .await
            .map_err(|error| remote_read_error(error, "remote device head"))?;
        let head: HeadDocument = serde_json::from_slice(&head_object.bytes)?;
        if head.generation_id != self.generation_id
            || head.device_id != remote_device_id
            || head.end_seq < 1
        {
            return Err(AppError::InvalidData(
                "remote head identity or sequence is invalid".into(),
            ));
        }

        let persisted_cursor = self
            .store
            .remote_cursor(&self.generation_id, remote_device_id)
            .await?;
        let cursor = persisted_cursor
            .as_ref()
            .map(|value| value.cursor_seq)
            .unwrap_or(0);
        let cursor_anchor = persisted_cursor
            .as_ref()
            .and_then(|value| value.anchor.as_ref());
        const MAX_PULL_BUNDLE_CHAIN_DEPTH: usize = 1000;
        let mut chain = Vec::new();
        let mut current = Some(head.clone());
        let mut released_v1_boundary = None;
        while let Some(document) = current {
            if chain.len() >= MAX_PULL_BUNDLE_CHAIN_DEPTH {
                return Err(AppError::InvalidData(
                    "remote bundle chain exceeded maximum depth limit".into(),
                ));
            }
            if document.end_seq < cursor {
                return Err(AppError::InvalidData(
                    "remote bundle chain skipped below the persisted cursor anchor".into(),
                ));
            }
            if document.end_seq == cursor {
                let anchor = cursor_anchor.ok_or_else(|| {
                    AppError::InvalidData("remote cursor is missing its object anchor".into())
                })?;
                require_document_matches_anchor(&document, anchor)?;
                break;
            }
            let downloaded = self
                .download_remote_bundle(&document, remote_device_id, compatibility)
                .await?;
            if downloaded.released_v1_unchained {
                released_v1_boundary = Some((document, downloaded));
                break;
            }
            let decoded = &downloaded.decoded;
            current = match (
                decoded.header.previous_path.clone(),
                decoded.header.previous_sha256.clone(),
                decoded.header.previous_end_seq,
            ) {
                (Some(path), Some(sha256), Some(end_seq)) => Some(HeadDocument {
                    generation_id: self.generation_id.clone(),
                    device_id: remote_device_id.to_owned(),
                    end_seq,
                    path,
                    sha256,
                }),
                (None, None, None) => None,
                _ => {
                    return Err(AppError::InvalidData(
                        "remote bundle chain fields are incomplete".into(),
                    ));
                }
            };
            chain.push(downloaded);
        }
        let requires_released_v1_reconstruction = released_v1_boundary.is_some();
        if let Some((boundary, downloaded)) = released_v1_boundary {
            chain = self
                .reconstruct_released_v1_history(
                    ReleasedV1Reconstruction {
                        devices_path,
                        remote_device_id,
                        legacy_head: &boundary,
                        cursor,
                        cursor_anchor,
                    },
                    downloaded,
                    chain,
                )
                .await?;
        }

        let mut pulled = 0;
        let ordered = if requires_released_v1_reconstruction {
            chain
        } else {
            chain.into_iter().rev().collect()
        };
        for mut downloaded in ordered {
            let expected = self
                .store
                .remote_cursor(&self.generation_id, remote_device_id)
                .await?
                .map(|value| value.cursor_seq)
                .unwrap_or(cursor);
            if downloaded.released_v1_unchained {
                let covered_start = expected
                    .checked_add(1)
                    .ok_or_else(|| AppError::InvalidData("released v1 cursor overflow".into()))?;
                downloaded.decoded.header.start_seq = covered_start;
                downloaded.decoded.contents.start_seq = covered_start;
                downloaded.decoded.header.previous_path = None;
                downloaded.decoded.header.previous_sha256 = None;
                downloaded.decoded.header.previous_end_seq = (expected > 0).then_some(expected);
                downloaded.decoded.contents.previous_path = None;
                downloaded.decoded.contents.previous_sha256 = None;
                downloaded.decoded.contents.previous_end_seq =
                    downloaded.decoded.header.previous_end_seq;
            }
            let anchor = RemoteObjectAnchor {
                end_seq: downloaded.decoded.header.end_seq,
                path: downloaded.path.clone(),
                sha256: downloaded.sha256.clone(),
            };
            let outcome = merger
                .apply_bundle(
                    &self.generation_id,
                    remote_device_id,
                    expected,
                    &downloaded.decoded,
                    &anchor,
                )
                .await?;
            pulled += outcome.applied;
        }
        Ok(pulled)
    }

    async fn reconstruct_released_v1_history(
        &self,
        reconstruction: ReleasedV1Reconstruction<'_>,
        legacy_head_bundle: RemoteBundleDownload,
        strict_suffix: Vec<RemoteBundleDownload>,
    ) -> Result<Vec<RemoteBundleDownload>> {
        let ReleasedV1Reconstruction {
            devices_path,
            remote_device_id,
            legacy_head,
            cursor,
            cursor_anchor,
        } = reconstruction;
        let bundles_path = devices_path
            .join(remote_device_id)
            .and_then(|path| path.join("bundles"))
            .map_err(|error| AppError::InvalidData(error.to_string()))?;
        let expected_prefix = format!("{}/", bundles_path.display());
        if !legacy_head.path.starts_with(&expected_prefix) {
            return Err(AppError::InvalidData(
                "released v1 head points outside its device bundle directory".into(),
            ));
        }
        let entries = self
            .backend
            .list_depth_one(&bundles_path)
            .await
            .map_err(|error| remote_read_error(error, "released v1 bundle listing"))?;
        let mut candidates = Vec::new();
        for entry in entries {
            if entry.is_collection || !entry.name.ends_with(".acmb") {
                continue;
            }
            let (start_seq, end_seq, sha256) = parse_bundle_object_name(&entry.name)?;
            if start_seq > legacy_head.end_seq || end_seq > legacy_head.end_seq {
                continue;
            }
            let path = bundles_path
                .join(&entry.name)
                .map_err(|error| AppError::InvalidData(error.to_string()))?;
            let path_display = path.display();
            let reference = HeadDocument {
                generation_id: self.generation_id.clone(),
                device_id: remote_device_id.to_owned(),
                end_seq,
                path: path_display.clone(),
                sha256: sha256.clone(),
            };
            let bundle = self
                .download_remote_bundle(
                    &reference,
                    remote_device_id,
                    Some(VaultCompatibility::ReleasedV1Writers),
                )
                .await?;
            if bundle.decoded.header.start_seq != start_seq {
                return Err(AppError::InvalidData(
                    "released v1 bundle filename does not match its sequence range".into(),
                ));
            }
            let has_no_previous = bundle.decoded.header.previous_path.is_none()
                && bundle.decoded.header.previous_sha256.is_none()
                && bundle.decoded.header.previous_end_seq.is_none();
            if bundle.released_v1_unchained || has_no_previous {
                candidates.push(ReleasedV1BundleCandidate {
                    path: path_display,
                    sha256,
                    bundle,
                });
            }
        }
        if !candidates.iter().any(|candidate| {
            candidate.path == legacy_head.path
                && candidate.sha256 == legacy_head.sha256
                && candidate.bundle.decoded.header.end_seq == legacy_head.end_seq
        }) {
            return Err(AppError::InvalidData(
                "released v1 authoritative head bundle was not found in immutable history".into(),
            ));
        }

        if cursor > 0 {
            let anchor = cursor_anchor.ok_or_else(|| {
                AppError::InvalidData("remote cursor is missing its object anchor".into())
            })?;
            if !candidates.iter().any(|candidate| {
                candidate.path == anchor.path
                    && candidate.sha256 == anchor.sha256
                    && candidate.bundle.decoded.header.end_seq == anchor.end_seq
            }) {
                return Err(AppError::SyncProtocol(
                    "released v1 immutable history no longer contains the persisted cursor anchor"
                        .into(),
                ));
            }
        }

        let mut unique_events = BTreeMap::<i64, BundleChange>::new();
        for candidate in &candidates {
            for change in &candidate.bundle.decoded.contents.changes {
                match unique_events.get(&change.local_seq) {
                    Some(existing) if existing == change => {}
                    Some(_) => {
                        return Err(AppError::SyncProtocol(
                            "released v1 bundle history is ambiguous: conflicting same sequence events"
                                .into(),
                        ));
                    }
                    None => {
                        unique_events.insert(change.local_seq, change.clone());
                    }
                }
            }
        }
        let pending = unique_events
            .into_iter()
            .filter_map(|(sequence, change)| (sequence > cursor).then_some(change))
            .collect::<Vec<_>>();
        if pending.is_empty()
            || pending.last().map(|change| change.local_seq) != Some(legacy_head.end_seq)
        {
            return Err(AppError::InvalidData(
                "released v1 authoritative history does not cover its head sequence".into(),
            ));
        }

        let mut decoded = legacy_head_bundle.decoded;
        decoded.contents.changes = pending;
        decoded.contents.start_seq = decoded
            .contents
            .changes
            .first()
            .map(|change| change.local_seq)
            .ok_or_else(|| AppError::InvalidData("released v1 history is empty".into()))?;
        decoded.header.start_seq = decoded.contents.start_seq;
        let mut reconstructed = vec![RemoteBundleDownload {
            decoded,
            released_v1_unchained: true,
            path: legacy_head.path.clone(),
            sha256: legacy_head.sha256.clone(),
        }];

        if let Some(oldest_current) = strict_suffix.last() {
            let header = &oldest_current.decoded.header;
            if header.previous_path.as_deref() != Some(legacy_head.path.as_str())
                || header.previous_sha256.as_deref() != Some(legacy_head.sha256.as_str())
                || header.previous_end_seq != Some(legacy_head.end_seq)
            {
                return Err(AppError::SyncProtocol(
                    "current bundle chain conflicts with reconstructed released v1 history".into(),
                ));
            }
        }
        reconstructed.extend(strict_suffix.into_iter().rev());
        Ok(reconstructed)
    }

    async fn download_verified_bundle(
        &self,
        document: &HeadDocument,
        remote_device_id: &str,
    ) -> Result<DecodedBundle> {
        let downloaded = self
            .download_remote_bundle(document, remote_device_id, None)
            .await?;
        if downloaded.released_v1_unchained {
            return Err(AppError::InvalidData(
                "released v1 unchained bundle is not valid in a current bundle chain".into(),
            ));
        }
        Ok(downloaded.decoded)
    }

    async fn download_remote_bundle(
        &self,
        document: &HeadDocument,
        remote_device_id: &str,
        compatibility: Option<VaultCompatibility>,
    ) -> Result<RemoteBundleDownload> {
        let path = RemotePath::parse(&document.path)
            .map_err(|error| AppError::InvalidData(error.to_string()))?;
        let object = self
            .backend
            .get(&path)
            .await
            .map_err(|error| remote_read_error(error, "remote bundle"))?;
        if sha256_hex(&object.bytes) != document.sha256 {
            return Err(AppError::InvalidData(
                "remote bundle SHA-256 does not match its chain reference".into(),
            ));
        }
        let (decoded, released_v1_unchained) = match open_bundle_protected(
            &object.bytes,
            &self.bundle_limits,
            self.protector.as_deref(),
        ) {
            Ok(decoded) => (decoded, false),
            Err(strict_error) => {
                match open_released_v1_unchained_bundle_protected(
                    &object.bytes,
                    &self.bundle_limits,
                    self.protector.as_deref(),
                ) {
                    Ok(decoded) if compatibility == Some(VaultCompatibility::ReleasedV1Writers) => {
                        (decoded, true)
                    }
                    Ok(_) => {
                        return Err(AppError::SyncProtocol(
                            "released v1 unchained bundle requires active compatibility".into(),
                        ));
                    }
                    Err(_) => return Err(strict_error),
                }
            }
        };
        if decoded.header.vault_id != self.vault_id
            || decoded.header.generation_id != self.generation_id
            || decoded.header.device_id != remote_device_id
            || decoded.header.end_seq != document.end_seq
        {
            return Err(AppError::InvalidData(
                "remote bundle does not match its chain reference".into(),
            ));
        }
        Ok(RemoteBundleDownload {
            decoded,
            released_v1_unchained,
            path: document.path.clone(),
            sha256: document.sha256.clone(),
        })
    }

    /// Rebuilds the local publication boundary under a new generation identifier.
    /// The existing generation remains untouched until the new immutable bundle has
    /// been uploaded and read back successfully.
    pub async fn rewrite_generation(&self, new_generation_id: &str) -> Result<SyncReport> {
        let current = load_versioned_identity(self.backend.as_ref()).await?;
        let operation_id = format!("rotation-{}", uuid::Uuid::new_v4().simple());
        self.rotate_generation_with_operation_policy(
            new_generation_id,
            current.protection,
            self.protector.clone(),
            &operation_id,
            true,
        )
        .await
        .into_result()
    }

    /// Pulls the current generation with this engine's read protector, writes a complete
    /// baseline with `new_protector`, then conditionally switches `v1/vault.json`.
    /// The old generation is intentionally retained for caller-controlled cleanup.
    pub async fn rotate_generation(
        &self,
        new_generation_id: &str,
        new_protection: VaultProtection,
        new_protector: Option<Arc<dyn PayloadProtector>>,
    ) -> Result<SyncReport> {
        let operation_id = format!("rotation-{}", uuid::Uuid::new_v4().simple());
        self.rotate_generation_with_operation(
            new_generation_id,
            new_protection,
            new_protector,
            &operation_id,
        )
        .await
        .into_result()
    }

    pub async fn rotate_generation_with_operation(
        &self,
        new_generation_id: &str,
        new_protection: VaultProtection,
        new_protector: Option<Arc<dyn PayloadProtector>>,
        operation_id: &str,
    ) -> RotationOutcome {
        self.rotate_generation_with_operation_policy(
            new_generation_id,
            new_protection,
            new_protector,
            operation_id,
            false,
        )
        .await
    }

    async fn rotate_generation_with_operation_policy(
        &self,
        new_generation_id: &str,
        new_protection: VaultProtection,
        new_protector: Option<Arc<dyn PayloadProtector>>,
        operation_id: &str,
        retire_released_v1_compatibility: bool,
    ) -> RotationOutcome {
        if new_generation_id.is_empty() || new_generation_id == self.generation_id {
            return RotationOutcome::Unknown {
                operation_id: operation_id.to_owned(),
                error: AppError::InvalidData("new generation id is invalid".into()),
            };
        }
        let target_algorithm = new_protector
            .as_deref()
            .map(PayloadProtector::algorithm)
            .unwrap_or(ProtectionAlgorithm::Plain);
        if new_protection.algorithm != target_algorithm {
            return RotationOutcome::Unknown {
                operation_id: operation_id.to_owned(),
                error: AppError::InvalidData(
                    "target protection does not match target protector".into(),
                ),
            };
        }
        let _guard = self.single_flight.lock().await;
        let current = match load_versioned_identity(self.backend.as_ref()).await {
            Ok(current) => current,
            Err(error) => {
                return RotationOutcome::Unknown {
                    operation_id: operation_id.to_owned(),
                    error,
                };
            }
        };
        if current.identity.vault_id != self.vault_id
            || current.identity.generation_id != self.generation_id
        {
            return RotationOutcome::Unknown {
                operation_id: operation_id.to_owned(),
                error: AppError::InvalidData(
                    "remote vault is not active for generation rotation".into(),
                ),
            };
        }
        if current.released_v1_compatibility_active() && !retire_released_v1_compatibility {
            return RotationOutcome::RolledBack {
                operation_id: operation_id.to_owned(),
                vault: current,
                error: AppError::Configuration(
                    "旧版同步兼容仍在生效；请先使用“重写云端存档”显式结束兼容".into(),
                ),
            };
        }
        let expected = current.active_document();
        if current.document() != expected {
            return RotationOutcome::Unknown {
                operation_id: operation_id.to_owned(),
                error: AppError::InvalidData(
                    "remote vault is not active for generation rotation".into(),
                ),
            };
        }
        if let Some(expected_protection) = &self.vault_protection
            && expected_protection != &current.protection
        {
            return RotationOutcome::RolledBack {
                operation_id: operation_id.to_owned(),
                vault: current,
                error: AppError::InvalidData(
                    "remote vault protection does not match this sync engine".into(),
                ),
            };
        }
        let started_at_ms = current_time_millis();
        let frozen = match begin_generation_freeze_owned_with_policy(
            self.backend.as_ref(),
            &expected,
            new_generation_id,
            new_protection,
            operation_id,
            &self.device_id,
            started_at_ms,
            started_at_ms.saturating_add(DEFAULT_MAINTENANCE_LEASE_MS),
            retire_released_v1_compatibility,
        )
        .await
        {
            Ok(frozen) => frozen,
            Err(error) => {
                return match load_versioned_identity(self.backend.as_ref()).await {
                    Ok(vault) if vault.document() == expected => RotationOutcome::RolledBack {
                        operation_id: operation_id.to_owned(),
                        vault,
                        error,
                    },
                    _ => RotationOutcome::Unknown {
                        operation_id: operation_id.to_owned(),
                        error,
                    },
                };
            }
        };
        let result = async {
            let mut report = self
                .pull_remote(PullPolicy::StrictMaintenance, current.compatibility)
                .await?;
            report.published += self
                .write_generation_baseline(new_generation_id, new_protector.as_deref())
                .await?;
            let ready = mark_frozen_generation_ready(self.backend.as_ref(), &frozen).await?;
            Ok::<_, AppError>((report, ready))
        }
        .await;
        let (report, ready) = match result {
            Ok(result) => result,
            Err(error) => {
                return self
                    .resolve_failed_rotation(operation_id, &frozen, SyncReport::default(), error)
                    .await;
            }
        };
        match activate_frozen_generation_outcome(self.backend.as_ref(), &ready).await {
            VaultUpdateOutcome::Committed(vault) => RotationOutcome::Committed {
                operation_id: operation_id.to_owned(),
                report,
                vault,
            },
            VaultUpdateOutcome::Rejected { error, .. } => {
                self.resolve_failed_rotation(operation_id, &ready, report, error)
                    .await
            }
            VaultUpdateOutcome::Unknown(error) => {
                self.resolve_unknown_rotation(operation_id, &ready, report, error)
                    .await
            }
        }
    }

    async fn resolve_failed_rotation(
        &self,
        operation_id: &str,
        frozen: &VersionedVaultIdentity,
        report: SyncReport,
        error: AppError,
    ) -> RotationOutcome {
        match rollback_frozen_generation(self.backend.as_ref(), frozen).await {
            Ok(vault)
                if vault.state == VaultState::Active
                    && vault.identity.generation_id == frozen.identity.generation_id =>
            {
                RotationOutcome::RolledBack {
                    operation_id: operation_id.to_owned(),
                    vault,
                    error,
                }
            }
            Ok(vault)
                if vault.state == VaultState::Active
                    && frozen_target_generation(frozen)
                        .is_some_and(|target| vault.identity.generation_id == target) =>
            {
                RotationOutcome::Committed {
                    operation_id: operation_id.to_owned(),
                    report,
                    vault,
                }
            }
            Ok(_) => RotationOutcome::Unknown {
                operation_id: operation_id.to_owned(),
                error,
            },
            Err(rollback) => {
                tracing::warn!(%rollback, "generation freeze rollback failed");
                self.resolve_unknown_rotation(operation_id, frozen, report, error)
                    .await
            }
        }
    }

    async fn resolve_unknown_rotation(
        &self,
        operation_id: &str,
        frozen: &VersionedVaultIdentity,
        report: SyncReport,
        error: AppError,
    ) -> RotationOutcome {
        match load_versioned_identity(self.backend.as_ref()).await {
            Ok(vault)
                if vault.state == VaultState::Active
                    && frozen_target_generation(frozen)
                        .is_some_and(|target| vault.identity.generation_id == target) =>
            {
                RotationOutcome::Committed {
                    operation_id: operation_id.to_owned(),
                    report,
                    vault,
                }
            }
            Ok(vault)
                if vault.state == VaultState::Active
                    && vault.identity == frozen.identity
                    && vault.protection == frozen.protection =>
            {
                RotationOutcome::RolledBack {
                    operation_id: operation_id.to_owned(),
                    vault,
                    error,
                }
            }
            _ => RotationOutcome::Unknown {
                operation_id: operation_id.to_owned(),
                error,
            },
        }
    }

    async fn write_generation_baseline(
        &self,
        new_generation_id: &str,
        new_protector: Option<&dyn PayloadProtector>,
    ) -> Result<usize> {
        let baseline = self.store.baseline_mutations().await?;
        if baseline.is_empty() {
            return Ok(0);
        }

        let mut previous_head: Option<HeadDocument> = None;
        let mut published = 0;
        while published < baseline.len() {
            let (contents, sealed, batch_len) = self.seal_largest_mutation_prefix(
                &baseline[published..],
                previous_head.as_ref(),
                new_generation_id,
                "baseline",
                new_protector,
            )?;
            let path = RemotePath::parse(&format!(
                "v1/generations/{new_generation_id}/devices/baseline/bundles/{}-{}-{}.acmb",
                contents.start_seq, contents.end_seq, sealed.file_sha256
            ))
            .map_err(|error| AppError::InvalidData(error.to_string()))?;
            self.ensure_parent_collections(&path).await?;
            match self.backend.put_immutable(&path, &sealed.bytes).await {
                Ok(()) => {}
                Err(error) if error.kind() == "precondition" => {
                    let existing = self.backend.get(&path).await.map_err(cloud_error)?;
                    if existing.bytes != sealed.bytes {
                        return Err(AppError::InvalidData(
                            "generation rewrite immutable bundle conflict".into(),
                        ));
                    }
                }
                Err(error) => return Err(cloud_error(error)),
            }
            let downloaded = self.backend.get(&path).await.map_err(cloud_error)?;
            if downloaded.bytes != sealed.bytes
                || sha256_hex(&downloaded.bytes) != sealed.file_sha256
            {
                return Err(AppError::InvalidData(
                    "generation rewrite verification failed".into(),
                ));
            }
            let decoded =
                open_bundle_protected(&downloaded.bytes, &self.bundle_limits, new_protector)?;
            if decoded.contents != contents {
                return Err(AppError::InvalidData(
                    "generation rewrite decoded bundle does not match its source".into(),
                ));
            }
            previous_head = Some(HeadDocument {
                generation_id: new_generation_id.to_owned(),
                device_id: "baseline".into(),
                end_seq: contents.end_seq,
                path: path.display(),
                sha256: sealed.file_sha256,
            });
            published += batch_len;
        }

        let head = previous_head.expect("a non-empty baseline always has a head");
        let head_path = RemotePath::parse(&format!(
            "v1/generations/{new_generation_id}/devices/baseline/head.json"
        ))
        .map_err(|error| AppError::InvalidData(error.to_string()))?;
        let head_bytes = serde_json::to_vec(&head)?;
        match self.backend.put_if_absent(&head_path, &head_bytes).await {
            Ok(()) => {}
            Err(error) if error.kind() == "precondition" => {
                let existing = self.backend.get(&head_path).await.map_err(cloud_error)?;
                if existing.bytes != head_bytes {
                    return Err(AppError::InvalidData(
                        "generation rewrite head already exists with different content".into(),
                    ));
                }
            }
            Err(error) => return Err(cloud_error(error)),
        }
        let stored_head = self.backend.get(&head_path).await.map_err(cloud_error)?;
        if stored_head.bytes != head_bytes {
            return Err(AppError::InvalidData(
                "generation rewrite head verification failed".into(),
            ));
        }
        Ok(published)
    }
    pub async fn publish_pending(&self) -> Result<SyncReport> {
        let pending = self
            .store
            .pending_mutations(MAX_MUTATIONS_PER_BUNDLE as i64)
            .await?;
        if pending.is_empty() {
            return Ok(SyncReport::default());
        }
        let (publication_vault, recovered_publication) = self.ensure_active_vault().await?;
        let head_path = self.head_path()?;
        let previous_head = match self.backend.get(&head_path).await {
            Ok(existing) => {
                let etag = existing
                    .etag
                    .ok_or_else(|| AppError::InvalidData("remote head has no ETag".into()))?;
                let head: HeadDocument = serde_json::from_slice(&existing.bytes)?;
                if head.generation_id != self.generation_id || head.device_id != self.device_id {
                    return Err(AppError::InvalidData(
                        "remote head identity mismatch".into(),
                    ));
                }
                Some((head, etag))
            }
            Err(error) if error.kind() == "not_found" => None,
            Err(error) => return Err(cloud_error(error)),
        };
        let already_published = previous_head
            .as_ref()
            .map(|(head, _etag)| {
                pending.partition_point(|mutation| mutation.local_seq <= head.end_seq)
            })
            .unwrap_or(0);
        let mut acknowledged = 0;
        let mut verified_head = false;
        if let Some((head, _etag)) = previous_head.as_ref()
            && self
                .store
                .published_bundle(&head.sha256)
                .await?
                .is_some_and(|bundle| bundle.stage == "staged")
        {
            self.download_verified_bundle(head, &self.device_id).await?;
            self.ensure_active_vault().await?;
            self.store
                .mark_bundle_published(&head.sha256, current_time_millis())
                .await?;
            verified_head = true;
        }
        if already_published > 0 {
            let head = &previous_head.as_ref().unwrap().0;
            if !verified_head {
                self.download_verified_bundle(head, &self.device_id).await?;
            }
            self.ensure_active_vault().await?;
            acknowledged += self
                .store
                .acknowledge_mutations(&pending[..already_published])
                .await?;
        }
        let pending = &pending[already_published..];
        if let Some((owner_device_id, published)) = recovered_publication {
            return Ok(SyncReport {
                published: usize::from(owner_device_id == self.device_id) * published,
                acknowledged,
                ..SyncReport::default()
            });
        }
        if pending.is_empty() {
            return Ok(SyncReport {
                acknowledged,
                ..SyncReport::default()
            });
        }
        let expected_start_seq = previous_head
            .as_ref()
            .map(|(head, _etag)| {
                head.end_seq
                    .checked_add(1)
                    .ok_or_else(|| AppError::InvalidData("remote sequence overflow".into()))
            })
            .transpose()?
            .unwrap_or(1);
        let staged = self
            .store
            .staged_bundle(&self.generation_id, &self.device_id, expected_start_seq)
            .await?;
        let (path, digest, bytes, end_seq, published_mutations) = if let Some(staged) = staged {
            let path = RemotePath::parse(&staged.object_path)
                .map_err(|error| AppError::InvalidData(error.to_string()))?;
            if sha256_hex(&staged.bundle_bytes) != staged.bundle_sha256 {
                return Err(AppError::InvalidData(
                    "staged bundle SHA-256 does not match its bytes".into(),
                ));
            }
            let decoded = open_bundle_protected(
                &staged.bundle_bytes,
                &self.bundle_limits,
                self.protector.as_deref(),
            )?;
            if decoded.header.vault_id != self.vault_id
                || decoded.header.generation_id != staged.generation_id
                || decoded.header.device_id != staged.device_id
                || decoded.header.start_seq != staged.start_seq
                || decoded.header.end_seq != staged.end_seq
                || staged.generation_id != self.generation_id
                || staged.device_id != self.device_id
            {
                return Err(AppError::InvalidData(
                    "staged bundle identity or range does not match its recovery record".into(),
                ));
            }
            let expected_previous = previous_head.as_ref().map(|(head, _etag)| {
                (
                    Some(head.path.as_str()),
                    Some(head.sha256.as_str()),
                    Some(head.end_seq),
                )
            });
            let actual_previous = (
                decoded.header.previous_path.as_deref(),
                decoded.header.previous_sha256.as_deref(),
                decoded.header.previous_end_seq,
            );
            if actual_previous != expected_previous.unwrap_or((None, None, None)) {
                return Err(AppError::InvalidData(
                    "staged bundle does not extend the current remote head".into(),
                ));
            }
            let expected_path = self.bundle_path(
                &SealedBundle {
                    bytes: Vec::new(),
                    file_sha256: staged.bundle_sha256.clone(),
                    header: decoded.header,
                },
                staged.start_seq,
                staged.end_seq,
            )?;
            if expected_path != path {
                return Err(AppError::InvalidData(
                    "staged bundle path does not match its content address".into(),
                ));
            }
            let published_mutations: Vec<PendingMutation> = decoded
                .contents
                .changes
                .into_iter()
                .map(|change| PendingMutation {
                    key: change.key,
                    local_seq: change.local_seq,
                    operation: change.operation,
                    version: change.version,
                    content_hash: change.content_hash,
                    snapshot: change.snapshot,
                })
                .collect();
            if published_mutations.is_empty()
                || published_mutations.len() > pending.len()
                || published_mutations != pending[..published_mutations.len()]
            {
                return Err(AppError::InvalidData(
                    "staged bundle does not match the current outbox prefix".into(),
                ));
            }
            (
                path,
                staged.bundle_sha256,
                staged.bundle_bytes,
                staged.end_seq,
                published_mutations,
            )
        } else {
            let (contents, sealed, published_count) = self.seal_largest_mutation_prefix(
                pending,
                previous_head.as_ref().map(|(head, _etag)| head),
                &self.generation_id,
                &self.device_id,
                self.protector.as_deref(),
            )?;
            let path = self.bundle_path(&sealed, contents.start_seq, contents.end_seq)?;
            let digest = sealed.file_sha256;
            let bytes = sealed.bytes;
            let end_seq = contents.end_seq;
            let published_mutations = pending[..published_count].to_vec();
            self.store
                .stage_bundle(
                    &digest,
                    &self.generation_id,
                    &self.device_id,
                    &path.display(),
                    contents.start_seq,
                    contents.end_seq,
                    &bytes,
                    current_time_millis(),
                )
                .await?;
            (path, digest, bytes, end_seq, published_mutations)
        };
        self.ensure_parent_collections(&path).await?;
        match self.backend.put_immutable(&path, &bytes).await {
            Ok(()) => {}
            Err(error) if error.kind() == "precondition" => {
                let existing = self.backend.get(&path).await.map_err(cloud_error)?;
                if sha256_hex(&existing.bytes) != digest {
                    return Err(AppError::InvalidData(
                        "immutable bundle hash conflict".into(),
                    ));
                }
            }
            Err(error) => return Err(cloud_error(error)),
        }
        let downloaded = self.backend.get(&path).await.map_err(cloud_error)?;
        if sha256_hex(&downloaded.bytes) != digest || downloaded.bytes != bytes {
            return Err(AppError::InvalidData(
                "uploaded bundle verification failed".into(),
            ));
        }
        let head = HeadDocument {
            generation_id: self.generation_id.clone(),
            device_id: self.device_id.clone(),
            end_seq,
            path: path.display(),
            sha256: digest.clone(),
        };
        let head_json = serde_json::to_string(&head)?;
        let started_at_ms = current_time_millis();
        let publishing = begin_head_publish(
            self.backend.as_ref(),
            &publication_vault,
            HeadPublishRequest {
                operation_id: format!("publish-{}", uuid::Uuid::new_v4().simple()),
                owner_device_id: self.device_id.clone(),
                started_at_ms,
                lease_expires_at_ms: started_at_ms.saturating_add(DEFAULT_MAINTENANCE_LEASE_MS),
                head_path: head_path.display(),
                expected_head_etag: previous_head.as_ref().map(|(_head, etag)| etag.clone()),
                replacement_head_json: head_json,
                published_mutation_count: published_mutations.len(),
            },
        )
        .await?;
        recover_head_publish(self.backend.as_ref(), &publishing).await?;
        self.store
            .mark_bundle_published(&digest, current_time_millis())
            .await?;
        acknowledged += self
            .store
            .acknowledge_mutations(&published_mutations)
            .await?;
        Ok(SyncReport {
            pulled: 0,
            published: published_mutations.len(),
            acknowledged,
        })
    }

    async fn ensure_active_vault(
        &self,
    ) -> Result<(VersionedVaultIdentity, Option<(String, usize)>)> {
        let engine_algorithm = self
            .protector
            .as_ref()
            .map(|protector| protector.algorithm())
            .unwrap_or(ProtectionAlgorithm::Plain);
        let mut current = load_versioned_identity(self.backend.as_ref()).await?;
        if current.identity.vault_id != self.vault_id
            || current.identity.generation_id != self.generation_id
            || current.protection.algorithm != engine_algorithm
            || self
                .vault_protection
                .as_ref()
                .is_some_and(|expected| expected != &current.protection)
        {
            return Err(AppError::InvalidData(
                "remote vault is not active for this sync generation".into(),
            ));
        }
        let mut recovered_publication = None;
        current = match &current.state {
            VaultState::Active => current,
            VaultState::Publishing {
                owner_device_id,
                published_mutation_count,
                ..
            } => {
                recovered_publication = Some((owner_device_id.clone(), *published_mutation_count));
                recover_head_publish(self.backend.as_ref(), &current).await?
            }
            VaultState::Frozen { .. } => {
                return Err(AppError::InvalidData(
                    "remote vault generation maintenance is active".into(),
                ));
            }
        };
        if current.state != VaultState::Active
            || current.identity.vault_id != self.vault_id
            || current.identity.generation_id != self.generation_id
            || current.protection.algorithm != engine_algorithm
        {
            return Err(AppError::InvalidData(
                "remote vault is not active for this sync generation".into(),
            ));
        }
        Ok((current, recovered_publication))
    }

    #[cfg(test)]
    fn contents_from_pending(
        &self,
        pending: &[PendingMutation],
        previous_head: Option<&HeadDocument>,
    ) -> Result<BundleContents> {
        self.contents_from_mutations_for(
            pending,
            previous_head,
            &self.generation_id,
            &self.device_id,
        )
    }

    fn contents_from_mutations_for(
        &self,
        pending: &[PendingMutation],
        previous_head: Option<&HeadDocument>,
        generation_id: &str,
        device_id: &str,
    ) -> Result<BundleContents> {
        let first = pending
            .first()
            .ok_or_else(|| AppError::InvalidData("cannot bundle empty outbox".into()))?;
        let last = pending.last().unwrap_or(first);
        let start_seq = match previous_head {
            Some(head) => head
                .end_seq
                .checked_add(1)
                .ok_or_else(|| AppError::InvalidData("remote sequence overflow".into()))?,
            None => 1,
        };
        if first.local_seq < start_seq {
            return Err(AppError::InvalidData(
                "pending mutation predates the remote publication head".into(),
            ));
        }
        Ok(BundleContents {
            vault_id: self.vault_id.clone(),
            generation_id: generation_id.to_owned(),
            device_id: device_id.to_owned(),
            start_seq,
            end_seq: last.local_seq,
            previous_path: previous_head.map(|head| head.path.clone()),
            previous_sha256: previous_head.map(|head| head.sha256.clone()),
            previous_end_seq: previous_head.map(|head| head.end_seq),
            changes: pending
                .iter()
                .map(|mutation| BundleChange {
                    local_seq: mutation.local_seq,
                    key: mutation.key.clone(),
                    operation: mutation.operation.clone(),
                    version: mutation.version.clone(),
                    content_hash: mutation.content_hash.clone(),
                    snapshot: mutation.snapshot.clone(),
                })
                .collect(),
        })
    }

    fn seal_largest_mutation_prefix(
        &self,
        pending: &[PendingMutation],
        previous_head: Option<&HeadDocument>,
        generation_id: &str,
        device_id: &str,
        protector: Option<&dyn PayloadProtector>,
    ) -> Result<(BundleContents, SealedBundle, usize)> {
        let candidate_count = pending.len().min(MAX_MUTATIONS_PER_BUNDLE);
        if candidate_count == 0 {
            return Err(AppError::InvalidData(
                "cannot select a bundle from an empty outbox".into(),
            ));
        }

        let single_contents = self.contents_from_mutations_for(
            &pending[..1],
            previous_head,
            generation_id,
            device_id,
        )?;
        let single_sealed =
            match seal_contents_with_protector(&single_contents, protector, &self.bundle_limits) {
                Ok(sealed) => sealed,
                Err(error) if is_bundle_limit_error(&error) => {
                    return Err(AppError::InvalidData(format!(
                        "single mutation exceeds bundle limits: {error}"
                    )));
                }
                Err(error) => return Err(error),
            };
        if candidate_count == 1 {
            return Ok((single_contents, single_sealed, 1));
        }

        let full_contents = self.contents_from_mutations_for(
            &pending[..candidate_count],
            previous_head,
            generation_id,
            device_id,
        )?;
        match seal_contents_with_protector(&full_contents, protector, &self.bundle_limits) {
            Ok(sealed) => return Ok((full_contents, sealed, candidate_count)),
            Err(error) if is_bundle_limit_error(&error) => {}
            Err(error) => return Err(error),
        }

        let mut low = 2;
        let mut high = candidate_count.saturating_sub(1);
        let mut best = (single_contents, single_sealed, 1);

        while low <= high {
            let mid = low + (high - low) / 2;
            let contents = self.contents_from_mutations_for(
                &pending[..mid],
                previous_head,
                generation_id,
                device_id,
            )?;
            match seal_contents_with_protector(&contents, protector, &self.bundle_limits) {
                Ok(sealed) => {
                    best = (contents, sealed, mid);
                    low = mid + 1;
                }
                Err(error) if is_bundle_limit_error(&error) => {
                    high = mid - 1;
                }
                Err(error) => return Err(error),
            }
        }

        Ok(best)
    }

    fn bundle_path(&self, sealed: &SealedBundle, start: i64, end: i64) -> Result<RemotePath> {
        RemotePath::parse(&format!(
            "v1/generations/{}/devices/{}/bundles/{start}-{end}-{}.acmb",
            self.generation_id, self.device_id, sealed.file_sha256
        ))
        .map_err(|error| AppError::InvalidData(error.to_string()))
    }

    fn head_path(&self) -> Result<RemotePath> {
        RemotePath::parse(&format!(
            "v1/generations/{}/devices/{}/head.json",
            self.generation_id, self.device_id
        ))
        .map_err(|error| AppError::InvalidData(error.to_string()))
    }

    async fn ensure_parent_collections(&self, path: &RemotePath) -> Result<()> {
        if path.segments().len() < 2 {
            return Ok(());
        }
        let mut parent = RemotePath::root();
        for segment in &path.segments()[..path.segments().len() - 1] {
            parent = parent
                .join(segment)
                .map_err(|e| AppError::InvalidData(e.to_string()))?;
            self.backend
                .create_collection(&parent)
                .await
                .map_err(cloud_error)?;
        }
        Ok(())
    }
}

fn parse_bundle_object_name(name: &str) -> Result<(i64, i64, String)> {
    let stem = name.strip_suffix(".acmb").ok_or_else(|| {
        AppError::InvalidData("released v1 bundle object has an invalid suffix".into())
    })?;
    let parts = stem.split('-').collect::<Vec<_>>();
    if parts.len() != 3 {
        return Err(AppError::InvalidData(
            "released v1 bundle object name is invalid".into(),
        ));
    }
    let start_seq = parts[0].parse::<i64>().map_err(|_| {
        AppError::InvalidData("released v1 bundle start sequence is invalid".into())
    })?;
    let end_seq = parts[1]
        .parse::<i64>()
        .map_err(|_| AppError::InvalidData("released v1 bundle end sequence is invalid".into()))?;
    let sha256 = parts[2];
    if start_seq < 1
        || end_seq < start_seq
        || sha256.len() != 64
        || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AppError::InvalidData(
            "released v1 bundle object identity is invalid".into(),
        ));
    }
    Ok((start_seq, end_seq, sha256.to_owned()))
}

fn require_document_matches_anchor(
    document: &HeadDocument,
    anchor: &RemoteObjectAnchor,
) -> Result<()> {
    if document.end_seq != anchor.end_seq
        || document.path != anchor.path
        || document.sha256 != anchor.sha256
    {
        return Err(AppError::SyncProtocol(
            "remote bundle predecessor does not match the persisted cursor anchor".into(),
        ));
    }
    Ok(())
}

fn seal_contents_with_protector(
    contents: &BundleContents,
    protector: Option<&dyn PayloadProtector>,
    limits: &BundleLimits,
) -> Result<SealedBundle> {
    match protector {
        Some(protector) => {
            let mut nonce = [0_u8; 24];
            rand::rng().fill_bytes(&mut nonce);
            seal_bundle_protected_with_limits(contents, protector, nonce, limits)
        }
        None => seal_bundle_with_limits(contents, limits),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn cloud_error(error: crate::sync::backend::CloudError) -> AppError {
    AppError::Cloud(error)
}

fn remote_read_error(error: crate::sync::backend::CloudError, context: &'static str) -> AppError {
    match error.kind() {
        "auth" | "offline" => cloud_error(error),
        _ => AppError::InvalidData(format!("{context} could not be read: {error}")),
    }
}

fn is_remote_data_error(error: &AppError) -> bool {
    matches!(
        error,
        AppError::Json(_) | AppError::Zip(_) | AppError::InvalidData(_)
    )
}

fn frozen_target_generation(frozen: &VersionedVaultIdentity) -> Option<&str> {
    match &frozen.state {
        VaultState::Frozen {
            target_generation_id,
            ..
        } => Some(target_generation_id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        database::{
            connection::{initialize_schema, register_sqlite_vec},
            import_sessions,
        },
        models::{NormalizedSession, S3CloudSyncSettings},
        sync::{
            backend::{CloudError, CloudErrorKind, CloudResult, RemoteEntry, RemoteObject},
            bundle::{
                BundleHeader, CompressionAlgorithm, ProtectionAlgorithm, SealedBundle, open_bundle,
                open_bundle_protected, seal_bundle,
            },
            crypto::Argon2idConfig,
            s3::S3Backend,
            test_s3_server::TestS3,
            test_server::TestWebDav,
            types::{
                BundleChange, BundleContents, EntityKey, EntityVersion, MutationOperation,
                NormalizedSessionSnapshot,
            },
            vault::{
                VaultDocument, VaultIdentity, VaultProtection, begin_generation_freeze,
                load_or_create_vault, load_versioned_identity,
            },
        },
    };
    use async_trait::async_trait;
    use serde::Deserialize;
    use serde_json::json;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::{
        collections::{BTreeMap, HashMap},
        io::{Cursor, Read},
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use tokio::sync::Notify;

    type EntityVersionRow = (String, String, String, i64, i64, String, Option<String>);

    struct FailFirstHeadWriteBackend {
        inner: Arc<S3Backend>,
        fail_head_write: AtomicBool,
        fail_vault_cas: AtomicBool,
        fail_activation_confirmation: AtomicBool,
        fail_next_vault_get: AtomicBool,
        bundle_objects: Mutex<HashMap<String, Vec<u8>>>,
    }

    struct PauseBeforePublishVaultCasBackend {
        inner: Arc<S3Backend>,
        publish_attempted: Arc<Notify>,
        release_publish: Arc<Notify>,
        pause_once: AtomicBool,
    }

    struct PauseBeforeVaultFreezeBackend {
        inner: Arc<S3Backend>,
        freeze_attempted: Arc<Notify>,
        release_freeze: Arc<Notify>,
        pause_once: AtomicBool,
    }

    impl PauseBeforeVaultFreezeBackend {
        fn new(inner: Arc<S3Backend>) -> Self {
            Self {
                inner,
                freeze_attempted: Arc::new(Notify::new()),
                release_freeze: Arc::new(Notify::new()),
                pause_once: AtomicBool::new(true),
            }
        }

        fn freeze_attempted(&self) -> Arc<Notify> {
            self.freeze_attempted.clone()
        }

        fn release(&self) {
            self.release_freeze.notify_one();
        }

        async fn pause_before_freeze(&self, path: &RemotePath) {
            if path.display() == "v1/vault.json" && self.pause_once.swap(false, Ordering::SeqCst) {
                self.freeze_attempted.notify_one();
                self.release_freeze.notified().await;
            }
        }
    }

    impl PauseBeforePublishVaultCasBackend {
        fn new(inner: Arc<S3Backend>) -> Self {
            Self {
                inner,
                publish_attempted: Arc::new(Notify::new()),
                release_publish: Arc::new(Notify::new()),
                pause_once: AtomicBool::new(true),
            }
        }

        fn publish_attempted(&self) -> Arc<Notify> {
            self.publish_attempted.clone()
        }

        fn release(&self) {
            self.release_publish.notify_one();
        }

        async fn pause_before_publish(&self, path: &RemotePath, bytes: &[u8]) {
            if path.display() != "v1/vault.json" || !self.pause_once.load(Ordering::SeqCst) {
                return;
            }
            let Ok(document) = serde_json::from_slice::<VaultDocument>(bytes) else {
                return;
            };
            if matches!(document.state, VaultState::Publishing { .. })
                && self.pause_once.swap(false, Ordering::SeqCst)
            {
                self.publish_attempted.notify_one();
                self.release_publish.notified().await;
            }
        }
    }

    #[async_trait]
    impl CloudBackend for PauseBeforePublishVaultCasBackend {
        async fn list_depth_one(&self, path: &RemotePath) -> CloudResult<Vec<RemoteEntry>> {
            self.inner.list_depth_one(path).await
        }

        async fn create_collection(&self, path: &RemotePath) -> CloudResult<()> {
            self.inner.create_collection(path).await
        }

        async fn get(&self, path: &RemotePath) -> CloudResult<RemoteObject> {
            self.inner.get(path).await
        }

        async fn put_immutable(&self, path: &RemotePath, bytes: &[u8]) -> CloudResult<()> {
            self.inner.put_immutable(path, bytes).await
        }

        async fn put_if_match(
            &self,
            path: &RemotePath,
            bytes: &[u8],
            etag: &str,
        ) -> CloudResult<()> {
            self.pause_before_publish(path, bytes).await;
            self.inner.put_if_match(path, bytes, etag).await
        }

        async fn put_if_absent(&self, path: &RemotePath, bytes: &[u8]) -> CloudResult<()> {
            self.inner.put_if_absent(path, bytes).await
        }

        async fn delete(&self, path: &RemotePath) -> CloudResult<()> {
            self.inner.delete(path).await
        }

        async fn test_capabilities(&self) -> CloudResult<()> {
            self.inner.test_capabilities().await
        }
    }

    #[async_trait]
    impl CloudBackend for PauseBeforeVaultFreezeBackend {
        async fn list_depth_one(&self, path: &RemotePath) -> CloudResult<Vec<RemoteEntry>> {
            self.inner.list_depth_one(path).await
        }

        async fn create_collection(&self, path: &RemotePath) -> CloudResult<()> {
            self.inner.create_collection(path).await
        }

        async fn get(&self, path: &RemotePath) -> CloudResult<RemoteObject> {
            self.inner.get(path).await
        }

        async fn put_immutable(&self, path: &RemotePath, bytes: &[u8]) -> CloudResult<()> {
            self.inner.put_immutable(path, bytes).await
        }

        async fn put_if_match(
            &self,
            path: &RemotePath,
            bytes: &[u8],
            etag: &str,
        ) -> CloudResult<()> {
            self.pause_before_freeze(path).await;
            self.inner.put_if_match(path, bytes, etag).await
        }

        async fn put_if_absent(&self, path: &RemotePath, bytes: &[u8]) -> CloudResult<()> {
            self.inner.put_if_absent(path, bytes).await
        }

        async fn delete(&self, path: &RemotePath) -> CloudResult<()> {
            self.inner.delete(path).await
        }

        async fn test_capabilities(&self) -> CloudResult<()> {
            self.inner.test_capabilities().await
        }
    }

    impl FailFirstHeadWriteBackend {
        fn new(inner: Arc<S3Backend>) -> Self {
            Self {
                inner,
                fail_head_write: AtomicBool::new(true),
                fail_vault_cas: AtomicBool::new(false),
                fail_activation_confirmation: AtomicBool::new(false),
                fail_next_vault_get: AtomicBool::new(false),
                bundle_objects: Mutex::new(HashMap::new()),
            }
        }

        fn failing_vault_cas(inner: Arc<S3Backend>) -> Self {
            Self {
                inner,
                fail_head_write: AtomicBool::new(false),
                fail_vault_cas: AtomicBool::new(false),
                fail_activation_confirmation: AtomicBool::new(false),
                fail_next_vault_get: AtomicBool::new(false),
                bundle_objects: Mutex::new(HashMap::new()),
            }
        }

        fn failing_activation_confirmation(inner: Arc<S3Backend>) -> Self {
            Self {
                inner,
                fail_head_write: AtomicBool::new(false),
                fail_vault_cas: AtomicBool::new(false),
                fail_activation_confirmation: AtomicBool::new(true),
                fail_next_vault_get: AtomicBool::new(false),
                bundle_objects: Mutex::new(HashMap::new()),
            }
        }

        fn arm_vault_cas_failure(&self) {
            self.fail_vault_cas.store(true, Ordering::SeqCst);
        }

        async fn bundle_objects(&self) -> HashMap<String, Vec<u8>> {
            self.bundle_objects.lock().await.clone()
        }

        fn should_fail_head_write(&self, path: &RemotePath) -> bool {
            path.display().ends_with("/head.json")
                && self.fail_head_write.swap(false, Ordering::SeqCst)
        }

        fn should_fail_vault_cas(&self, path: &RemotePath) -> bool {
            path.display() == "v1/vault.json" && self.fail_vault_cas.swap(false, Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl CloudBackend for FailFirstHeadWriteBackend {
        async fn list_depth_one(&self, path: &RemotePath) -> CloudResult<Vec<RemoteEntry>> {
            self.inner.list_depth_one(path).await
        }

        async fn create_collection(&self, path: &RemotePath) -> CloudResult<()> {
            self.inner.create_collection(path).await
        }

        async fn get(&self, path: &RemotePath) -> CloudResult<RemoteObject> {
            if path.display() == "v1/vault.json"
                && self.fail_next_vault_get.swap(false, Ordering::SeqCst)
            {
                return Err(CloudError::new(
                    CloudErrorKind::Offline,
                    "injected vault confirmation read failure",
                ));
            }
            self.inner.get(path).await
        }

        async fn put_immutable(&self, path: &RemotePath, bytes: &[u8]) -> CloudResult<()> {
            let result = self.inner.put_immutable(path, bytes).await;
            if result.is_ok() && path.display().contains("/bundles/") {
                self.bundle_objects
                    .lock()
                    .await
                    .insert(path.display(), bytes.to_vec());
            }
            result
        }

        async fn put_if_match(
            &self,
            path: &RemotePath,
            bytes: &[u8],
            etag: &str,
        ) -> CloudResult<()> {
            if self.should_fail_head_write(path) || self.should_fail_vault_cas(path) {
                return Err(CloudError::new(
                    CloudErrorKind::Offline,
                    "injected conditional write failure",
                ));
            }
            let result = self.inner.put_if_match(path, bytes, etag).await;
            if result.is_ok()
                && path.display() == "v1/vault.json"
                && self.fail_activation_confirmation.load(Ordering::SeqCst)
                && serde_json::from_slice::<VaultDocument>(bytes).is_ok_and(|document| {
                    document.state == VaultState::Active
                        && document.identity.generation_id == "generation-confirmed"
                })
            {
                self.fail_activation_confirmation
                    .store(false, Ordering::SeqCst);
                self.fail_next_vault_get.store(true, Ordering::SeqCst);
            }
            result
        }

        async fn put_if_absent(&self, path: &RemotePath, bytes: &[u8]) -> CloudResult<()> {
            if self.should_fail_head_write(path) {
                return Err(CloudError::new(
                    CloudErrorKind::Offline,
                    "injected head write failure",
                ));
            }
            self.inner.put_if_absent(path, bytes).await
        }

        async fn delete(&self, path: &RemotePath) -> CloudResult<()> {
            self.inner.delete(path).await
        }

        async fn test_capabilities(&self) -> CloudResult<()> {
            self.inner.test_capabilities().await
        }
    }

    async fn test_store(device_id: &str) -> (SyncStore, sqlx::SqlitePool) {
        register_sqlite_vec();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        initialize_schema(&pool).await.unwrap();
        let store = SyncStore::new(pool.clone());
        store.initialize_device(device_id, device_id).await.unwrap();
        (store, pool)
    }

    fn snapshot(index: usize, title: &str) -> NormalizedSessionSnapshot {
        NormalizedSessionSnapshot {
            key: EntityKey {
                platform: "chat".into(),
                platform_session_id: format!("remote-{index}"),
            },
            title: title.into(),
            created_at: None,
            updated_at: None,
            imported_at: "2026-07-29T00:00:00Z".into(),
            raw_data: json!({"fixture": index}),
            messages: vec![],
        }
    }

    #[derive(serde::Serialize)]
    struct ReleasedBundleManifest<'a> {
        vault_id: &'a str,
        generation_id: &'a str,
        device_id: &'a str,
        start_seq: i64,
        end_seq: i64,
        previous_path: Option<&'a str>,
        previous_sha256: Option<&'a str>,
        previous_end_seq: Option<i64>,
        change_count: usize,
        changes_sha256: String,
    }

    #[derive(serde::Serialize)]
    struct ReleasedBundleChangeWire<'a> {
        local_seq: i64,
        key: &'a EntityKey,
        operation: &'a MutationOperation,
        version: &'a EntityVersion,
        content_hash: Option<&'a str>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ReleasedReaderManifest {
        vault_id: String,
        generation_id: String,
        device_id: String,
        start_seq: i64,
        end_seq: i64,
        previous_path: Option<String>,
        previous_sha256: Option<String>,
        previous_end_seq: Option<i64>,
        change_count: usize,
        changes_sha256: String,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    enum ReleasedReaderCompression {
        Zstandard,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    enum ReleasedReaderProtection {
        Plain,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ReleasedReaderHeader {
        vault_id: String,
        generation_id: String,
        device_id: String,
        start_seq: i64,
        end_seq: i64,
        previous_path: Option<String>,
        previous_sha256: Option<String>,
        previous_end_seq: Option<i64>,
        compression: ReleasedReaderCompression,
        protection: ReleasedReaderProtection,
        nonce: Option<String>,
        payload_length: u64,
        payload_sha256: String,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct ReleasedReaderEntityKey {
        platform: String,
        platform_session_id: String,
    }

    #[derive(Debug, Deserialize)]
    struct ReleasedReaderEntityVersion {
        wall_ms: i64,
        counter: i64,
        device_id: String,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    enum ReleasedReaderMutationOperation {
        Upsert,
        Delete,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct ReleasedReaderMessageSnapshot {
        role: String,
        content: String,
        metadata: serde_json::Value,
        created_at: Option<String>,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct ReleasedReaderSessionSnapshot {
        key: ReleasedReaderEntityKey,
        title: String,
        created_at: Option<String>,
        updated_at: Option<String>,
        imported_at: String,
        raw_data: serde_json::Value,
        messages: Vec<ReleasedReaderMessageSnapshot>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ReleasedReaderChangeWire {
        local_seq: i64,
        key: ReleasedReaderEntityKey,
        operation: ReleasedReaderMutationOperation,
        version: ReleasedReaderEntityVersion,
        content_hash: Option<String>,
    }

    #[derive(Debug)]
    struct ReleasedReaderChange {
        snapshot: Option<ReleasedReaderSessionSnapshot>,
    }

    #[derive(Debug)]
    struct ReleasedReaderBundle {
        changes: Vec<ReleasedReaderChange>,
    }

    fn append_released_bundle_file(
        builder: &mut tar::Builder<&mut Vec<u8>>,
        path: &str,
        bytes: &[u8],
    ) {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, path, Cursor::new(bytes))
            .unwrap();
    }

    fn seal_released_v1_unchained_bundle(contents: &BundleContents) -> SealedBundle {
        assert!(contents.previous_path.is_none());
        assert!(contents.previous_sha256.is_none());
        assert!(contents.previous_end_seq.is_none());

        let mut changes = Vec::new();
        let mut sessions = BTreeMap::new();
        for change in &contents.changes {
            serde_json::to_writer(
                &mut changes,
                &ReleasedBundleChangeWire {
                    local_seq: change.local_seq,
                    key: &change.key,
                    operation: &change.operation,
                    version: &change.version,
                    content_hash: change.content_hash.as_deref(),
                },
            )
            .unwrap();
            changes.push(b'\n');
            if let (Some(content_hash), Some(snapshot)) = (&change.content_hash, &change.snapshot) {
                sessions
                    .entry(content_hash.clone())
                    .or_insert_with(|| serde_json::to_vec(snapshot).unwrap());
            }
        }
        let manifest = serde_json::to_vec(&ReleasedBundleManifest {
            vault_id: &contents.vault_id,
            generation_id: &contents.generation_id,
            device_id: &contents.device_id,
            start_seq: contents.start_seq,
            end_seq: contents.end_seq,
            previous_path: None,
            previous_sha256: None,
            previous_end_seq: None,
            change_count: contents.changes.len(),
            changes_sha256: sha256_hex(&changes),
        })
        .unwrap();
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            append_released_bundle_file(&mut builder, "bundle.json", &manifest);
            append_released_bundle_file(&mut builder, "changes.ndjson", &changes);
            for (content_hash, bytes) in sessions {
                append_released_bundle_file(
                    &mut builder,
                    &format!("sessions/{content_hash}.json"),
                    &bytes,
                );
            }
            builder.finish().unwrap();
        }
        let payload = zstd::stream::encode_all(Cursor::new(tar_bytes), 3).unwrap();
        let header = BundleHeader {
            vault_id: contents.vault_id.clone(),
            generation_id: contents.generation_id.clone(),
            device_id: contents.device_id.clone(),
            start_seq: contents.start_seq,
            end_seq: contents.end_seq,
            previous_path: None,
            previous_sha256: None,
            previous_end_seq: None,
            compression: CompressionAlgorithm::Zstandard,
            protection: ProtectionAlgorithm::Plain,
            nonce: None,
            payload_length: payload.len() as u64,
            payload_sha256: sha256_hex(&payload),
        };
        let header_bytes = serde_json::to_vec(&header).unwrap();
        let mut bytes = Vec::with_capacity(9 + header_bytes.len() + payload.len());
        bytes.extend_from_slice(b"ACMB");
        bytes.push(1);
        bytes.extend_from_slice(&(header_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&header_bytes);
        bytes.extend_from_slice(&payload);
        SealedBundle {
            file_sha256: sha256_hex(&bytes),
            bytes,
            header,
        }
    }

    /// Test-only validator pinned to the reader shipped at the current committed HEAD.
    /// It intentionally does not call the production parser under test.
    fn open_with_released_v1_reader(bytes: &[u8]) -> Result<ReleasedReaderBundle> {
        if bytes.len() < 9 || &bytes[..4] != b"ACMB" || bytes[4] != 1 {
            return Err(AppError::InvalidData(
                "released reader rejected the bundle envelope".into(),
            ));
        }
        let header_len = u32::from_be_bytes(bytes[5..9].try_into().unwrap()) as usize;
        let header_end = 9_usize
            .checked_add(header_len)
            .ok_or_else(|| AppError::InvalidData("released header length overflow".into()))?;
        if header_end > bytes.len() {
            return Err(AppError::InvalidData(
                "released reader rejected a truncated header".into(),
            ));
        }
        let header: ReleasedReaderHeader = serde_json::from_slice(&bytes[9..header_end])?;
        let payload = &bytes[header_end..];
        if header.vault_id.is_empty()
            || header.generation_id.is_empty()
            || header.device_id.is_empty()
            || header.start_seq < 0
            || header.end_seq < header.start_seq
            || header.compression != ReleasedReaderCompression::Zstandard
            || header.protection != ReleasedReaderProtection::Plain
            || header.nonce.is_some()
            || header.payload_length != payload.len() as u64
            || header.payload_sha256 != sha256_hex(payload)
        {
            return Err(AppError::InvalidData(
                "released reader rejected bundle protection or payload identity".into(),
            ));
        }
        match (
            header.previous_path.as_deref(),
            header.previous_sha256.as_deref(),
            header.previous_end_seq,
        ) {
            (None, None, None) => {}
            (Some(path), Some(hash), Some(end_seq))
                if !path.is_empty()
                    && path.ends_with(".acmb")
                    && hash.len() == 64
                    && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && end_seq.checked_add(1) == Some(header.start_seq) => {}
            _ => {
                return Err(AppError::InvalidData(
                    "released reader rejected previous bundle fields".into(),
                ));
            }
        }

        let tar_bytes = zstd::stream::decode_all(Cursor::new(payload))?;
        let mut archive = tar::Archive::new(Cursor::new(tar_bytes));
        let mut files = BTreeMap::<String, Vec<u8>>::new();
        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry
                .path()?
                .to_str()
                .ok_or_else(|| AppError::InvalidData("released tar path is invalid".into()))?
                .to_owned();
            if files.contains_key(&path) {
                return Err(AppError::InvalidData(
                    "released tar contains a duplicate path".into(),
                ));
            }
            let mut contents = Vec::new();
            entry.read_to_end(&mut contents)?;
            files.insert(path, contents);
        }
        let manifest: ReleasedReaderManifest = serde_json::from_slice(
            files
                .get("bundle.json")
                .ok_or_else(|| AppError::InvalidData("released manifest is missing".into()))?,
        )?;
        let changes_bytes = files
            .get("changes.ndjson")
            .ok_or_else(|| AppError::InvalidData("released changes are missing".into()))?;
        if manifest.vault_id != header.vault_id
            || manifest.generation_id != header.generation_id
            || manifest.device_id != header.device_id
            || manifest.start_seq != header.start_seq
            || manifest.end_seq != header.end_seq
            || manifest.previous_path != header.previous_path
            || manifest.previous_sha256 != header.previous_sha256
            || manifest.previous_end_seq != header.previous_end_seq
            || manifest.changes_sha256 != sha256_hex(changes_bytes)
        {
            return Err(AppError::InvalidData(
                "released manifest does not match its envelope".into(),
            ));
        }
        let mut changes = Vec::new();
        let mut change_sequences = Vec::new();
        for line in changes_bytes.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            let wire: ReleasedReaderChangeWire = serde_json::from_slice(line)?;
            if wire.version.wall_ms < 0
                || wire.version.counter < 0
                || wire.version.device_id.is_empty()
            {
                return Err(AppError::InvalidData(
                    "released entity version is invalid".into(),
                ));
            }
            let snapshot = match wire.operation {
                ReleasedReaderMutationOperation::Upsert => {
                    let hash = wire.content_hash.as_deref().ok_or_else(|| {
                        AppError::InvalidData("released upsert hash is missing".into())
                    })?;
                    let snapshot_bytes =
                        files.get(&format!("sessions/{hash}.json")).ok_or_else(|| {
                            AppError::InvalidData("released snapshot is missing".into())
                        })?;
                    let snapshot: ReleasedReaderSessionSnapshot =
                        serde_json::from_slice(snapshot_bytes)?;
                    if snapshot.key != wire.key || sha256_hex(snapshot_bytes) != hash {
                        return Err(AppError::InvalidData(
                            "released snapshot identity is invalid".into(),
                        ));
                    }
                    Some(snapshot)
                }
                ReleasedReaderMutationOperation::Delete if wire.content_hash.is_none() => None,
                ReleasedReaderMutationOperation::Delete => {
                    return Err(AppError::InvalidData(
                        "released delete unexpectedly carries content".into(),
                    ));
                }
            };
            change_sequences.push(wire.local_seq);
            changes.push(ReleasedReaderChange { snapshot });
        }
        if changes.len() != manifest.change_count
            || change_sequences.first().copied() != Some(header.start_seq)
            || change_sequences.last().copied() != Some(header.end_seq)
            || change_sequences.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(AppError::InvalidData(
                "released changes do not match the sequence range".into(),
            ));
        }
        Ok(ReleasedReaderBundle { changes })
    }

    fn released_v1_contents_for(
        vault_id: &str,
        generation_id: &str,
        sequence: i64,
        index: usize,
        title: &str,
    ) -> BundleContents {
        let snapshot = snapshot(index, title);
        let content_hash = sha256_hex(&serde_json::to_vec(&snapshot).unwrap());
        BundleContents {
            vault_id: vault_id.into(),
            generation_id: generation_id.into(),
            device_id: "device-old".into(),
            start_seq: sequence,
            end_seq: sequence,
            previous_path: None,
            previous_sha256: None,
            previous_end_seq: None,
            changes: vec![BundleChange {
                local_seq: sequence,
                key: snapshot.key.clone(),
                operation: MutationOperation::Upsert,
                version: EntityVersion::new(sequence, 0, "device-old"),
                content_hash: Some(content_hash),
                snapshot: Some(snapshot),
            }],
        }
    }

    fn released_v1_contents(sequence: i64, index: usize, title: &str) -> BundleContents {
        released_v1_contents_for("default", "generation-1", sequence, index, title)
    }

    async fn publish_released_v1_bundle<B: CloudBackend + ?Sized>(
        backend: &B,
        contents: &BundleContents,
    ) -> HeadDocument {
        let sealed = seal_released_v1_unchained_bundle(contents);
        let bundles_path = RemotePath::parse(&format!(
            "v1/generations/{}/devices/{}/bundles",
            contents.generation_id, contents.device_id
        ))
        .unwrap();
        backend.create_collection(&bundles_path).await.unwrap();
        let bundle_path = bundles_path
            .join(&format!(
                "{}-{}-{}.acmb",
                contents.start_seq, contents.end_seq, sealed.file_sha256
            ))
            .unwrap();
        backend
            .put_immutable(&bundle_path, &sealed.bytes)
            .await
            .unwrap();
        let head = HeadDocument {
            generation_id: contents.generation_id.clone(),
            device_id: contents.device_id.clone(),
            end_seq: contents.end_seq,
            path: bundle_path.display(),
            sha256: sealed.file_sha256,
        };
        let head_path = RemotePath::parse(&format!(
            "v1/generations/{}/devices/{}/head.json",
            contents.generation_id, contents.device_id
        ))
        .unwrap();
        let head_bytes = serde_json::to_vec(&head).unwrap();
        match backend.get(&head_path).await {
            Ok(existing) => {
                backend
                    .put_if_match(&head_path, &head_bytes, existing.etag.as_deref().unwrap())
                    .await
                    .unwrap();
            }
            Err(error) if error.kind() == "not_found" => {
                backend
                    .put_if_absent(&head_path, &head_bytes)
                    .await
                    .unwrap();
            }
            Err(error) => panic!("unexpected released head read error: {error}"),
        }
        head
    }

    async fn publish_current_bundle<B: CloudBackend + ?Sized>(
        backend: &B,
        contents: &BundleContents,
    ) -> HeadDocument {
        let sealed = seal_bundle(contents).unwrap();
        let bundles_path = RemotePath::parse(&format!(
            "v1/generations/{}/devices/{}/bundles",
            contents.generation_id, contents.device_id
        ))
        .unwrap();
        backend.create_collection(&bundles_path).await.unwrap();
        let bundle_path = bundles_path
            .join(&format!(
                "{}-{}-{}.acmb",
                contents.start_seq, contents.end_seq, sealed.file_sha256
            ))
            .unwrap();
        backend
            .put_immutable(&bundle_path, &sealed.bytes)
            .await
            .unwrap();
        let head = HeadDocument {
            generation_id: contents.generation_id.clone(),
            device_id: contents.device_id.clone(),
            end_seq: contents.end_seq,
            path: bundle_path.display(),
            sha256: sealed.file_sha256,
        };
        let head_path = RemotePath::parse(&format!(
            "v1/generations/{}/devices/{}/head.json",
            contents.generation_id, contents.device_id
        ))
        .unwrap();
        let existing = backend.get(&head_path).await.unwrap();
        backend
            .put_if_match(
                &head_path,
                &serde_json::to_vec(&head).unwrap(),
                existing.etag.as_deref().unwrap(),
            )
            .await
            .unwrap();
        head
    }

    fn noisy_snapshot(index: usize, title: &str) -> NormalizedSessionSnapshot {
        let mut snapshot = snapshot(index, title);
        let payload = (0..256)
            .map(|chunk| sha256_hex(format!("{index}-{chunk}-{title}").as_bytes()))
            .collect::<String>();
        snapshot.raw_data = json!({"payload": payload});
        snapshot
    }

    fn noisy_normalized_session(index: usize, title: &str) -> NormalizedSession {
        let mut session = normalized_session(index, title, "local");
        session.raw_data = noisy_snapshot(index, title).raw_data;
        session
    }

    fn normalized_session(index: usize, title: &str, local_prefix: &str) -> NormalizedSession {
        NormalizedSession {
            id: format!("{local_prefix}-{index}"),
            platform: "chat".into(),
            platform_session_id: format!("remote-{index}"),
            title: title.into(),
            created_at: None,
            updated_at: None,
            imported_at: "2026-07-29T00:00:00Z".into(),
            messages: vec![],
            raw_data: json!({"fixture": index}),
        }
    }

    fn s3_backend(server: &TestS3) -> Arc<S3Backend> {
        let settings = S3CloudSyncSettings {
            endpoint_url: server.endpoint().into(),
            region: "us-east-1".into(),
            bucket: "archive".into(),
            prefix: "engine-tests".into(),
            force_path_style: true,
        };
        Arc::new(S3Backend::new(&settings, "AKID", "secret-key", None).unwrap())
    }

    fn test_protector(passphrase: &str) -> Arc<dyn PayloadProtector> {
        test_protection(passphrase)
            .derive_protector("vault", passphrase)
            .unwrap()
            .unwrap()
    }

    fn test_protection(passphrase: &str) -> VaultProtection {
        VaultProtection::encrypted_with_config(
            "vault",
            passphrase,
            Argon2idConfig {
                salt: [11; 16],
                memory_kib: 8 * 1024,
                iterations: 2,
                parallelism: 1,
            },
        )
        .unwrap()
    }

    async fn initialize_test_vault<B: CloudBackend + ?Sized>(
        backend: &B,
        vault_id: &str,
        generation_id: &str,
        protection: VaultProtection,
    ) {
        load_or_create_vault(
            backend,
            VaultDocument::active(
                VaultIdentity {
                    format_version: 2,
                    vault_id: vault_id.to_owned(),
                    generation_id: generation_id.to_owned(),
                },
                protection,
            ),
        )
        .await
        .unwrap();
    }

    async fn initialize_released_v1_test_vault<B: CloudBackend + ?Sized>(backend: &B) {
        load_or_create_vault(
            backend,
            VaultDocument::released_v1_compatible(VaultIdentity {
                format_version: 2,
                vault_id: "default".into(),
                generation_id: "generation-1".into(),
            }),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn ordinary_vault_rejects_released_v1_unchained_fallback() {
        let server = TestWebDav::start("user", "pass").await;
        let backend = Arc::new(server.client("user", "pass").unwrap());
        publish_released_v1_bundle(
            backend.as_ref(),
            &released_v1_contents_for(
                "vault-current",
                "generation-current",
                2,
                2,
                "must-not-import",
            ),
        )
        .await;
        initialize_test_vault(
            backend.as_ref(),
            "vault-current",
            "generation-current",
            VaultProtection::plain(),
        )
        .await;
        let (store, pool) = test_store("device-current").await;
        let engine = SyncEngine::new(
            store,
            backend,
            "vault-current",
            "generation-current",
            "device-current",
        );

        let error = engine.run_once(SyncTrigger::Manual).await.unwrap_err();

        assert!(
            matches!(error, AppError::SyncProtocol(ref message) if message.contains("compatibility")),
            "{error:?}"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sessions")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn retired_generation_rejects_released_v1_unchained_fallback() {
        let server = TestWebDav::start("user", "pass").await;
        let backend = Arc::new(server.client("user", "pass").unwrap());
        publish_released_v1_bundle(
            backend.as_ref(),
            &released_v1_contents_for(
                "default",
                "generation-retired",
                2,
                2,
                "old-writer-after-retirement",
            ),
        )
        .await;
        initialize_test_vault(
            backend.as_ref(),
            "default",
            "generation-retired",
            VaultProtection::plain(),
        )
        .await;
        let (store, pool) = test_store("device-retired").await;
        let engine = SyncEngine::new(
            store,
            backend,
            "default",
            "generation-retired",
            "device-retired",
        );

        let error = engine.run_once(SyncTrigger::Manual).await.unwrap_err();

        assert!(
            matches!(error, AppError::SyncProtocol(ref message) if message.contains("compatibility")),
            "{error:?}"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sessions")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn released_v1_upgrade_imports_remote_only_data_and_publishes_readable_v1_output() {
        let server = TestWebDav::start("user", "pass").await;
        let backend = Arc::new(server.client("user", "pass").unwrap());
        publish_released_v1_bundle(
            backend.as_ref(),
            &released_v1_contents(1, 1, "released-remote-only"),
        )
        .await;
        let v1_vault_path = RemotePath::parse("v1/vault.json").unwrap();
        assert!(
            backend
                .get(&v1_vault_path)
                .await
                .is_err_and(|error| error.kind() == "not_found")
        );

        initialize_released_v1_test_vault(backend.as_ref()).await;
        let (store, pool) = test_store("device-upgraded").await;
        let engine = SyncEngine::new(
            store,
            backend.clone(),
            "default",
            "generation-1",
            "device-upgraded",
        );

        let report = engine.run_once(SyncTrigger::Manual).await.unwrap();
        assert_eq!(report.pulled, 1);
        let remote_title: String =
            sqlx::query_scalar("SELECT title FROM sessions WHERE platform_session_id = ?")
                .bind("remote-1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(remote_title, "released-remote-only");
        backend.get(&v1_vault_path).await.unwrap();
        assert!(
            backend
                .get(&RemotePath::parse("v2/vault.json").unwrap())
                .await
                .is_err_and(|error| error.kind() == "not_found")
        );

        import_sessions(
            &pool,
            &[normalized_session(2, "from-upgraded-client", "upgraded")],
            true,
        )
        .await
        .unwrap();
        let publish = engine.run_once(SyncTrigger::Manual).await.unwrap();
        assert_eq!((publish.published, publish.acknowledged), (1, 1));
        let upgraded_head_path =
            RemotePath::parse("v1/generations/generation-1/devices/device-upgraded/head.json")
                .unwrap();
        let upgraded_head: HeadDocument =
            serde_json::from_slice(&backend.get(&upgraded_head_path).await.unwrap().bytes).unwrap();
        assert!(
            upgraded_head
                .path
                .starts_with("v1/generations/generation-1/devices/device-upgraded/bundles/")
        );
        let upgraded_bundle = backend
            .get(&RemotePath::parse(&upgraded_head.path).unwrap())
            .await
            .unwrap();
        let released_reader_view = open_with_released_v1_reader(&upgraded_bundle.bytes)
            .expect("released v1 reader can decode upgraded output");
        assert!(released_reader_view.changes.iter().any(|change| {
            change
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.title == "from-upgraded-client")
        }));
    }

    #[tokio::test]
    async fn released_v1_upgrade_accepts_coalesced_sequence_gaps() {
        let server = TestWebDav::start("user", "pass").await;
        let backend = Arc::new(server.client("user", "pass").unwrap());
        let first = seal_released_v1_unchained_bundle(&released_v1_contents(
            2,
            2,
            "released-starts-at-two",
        ));
        open_with_released_v1_reader(&first.bytes)
            .expect("committed released reader accepts a first bundle above sequence one");
        publish_released_v1_bundle(
            backend.as_ref(),
            &released_v1_contents(2, 2, "released-starts-at-two"),
        )
        .await;
        let second =
            seal_released_v1_unchained_bundle(&released_v1_contents(4, 4, "released-gap-at-three"));
        open_with_released_v1_reader(&second.bytes)
            .expect("committed released reader accepts a later coalesced range gap");
        publish_released_v1_bundle(
            backend.as_ref(),
            &released_v1_contents(4, 4, "released-gap-at-three"),
        )
        .await;
        initialize_released_v1_test_vault(backend.as_ref()).await;
        let (store, pool) = test_store("device-upgraded").await;
        let engine = SyncEngine::new(store, backend, "default", "generation-1", "device-upgraded");

        let report = engine
            .pull_remote(
                PullPolicy::StrictMaintenance,
                Some(VaultCompatibility::ReleasedV1Writers),
            )
            .await
            .unwrap();

        assert_eq!(report.pulled, 2);
        let imported: Vec<(String, String)> = sqlx::query_as(
            "SELECT platform_session_id, title FROM sessions ORDER BY platform_session_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            imported,
            vec![
                ("remote-2".into(), "released-starts-at-two".into()),
                ("remote-4".into(), "released-gap-at-three".into()),
            ]
        );
    }

    #[tokio::test]
    async fn released_v1_recovery_rejects_conflicting_same_sequence_events() {
        let server = TestWebDav::start("user", "pass").await;
        let backend = Arc::new(server.client("user", "pass").unwrap());
        publish_released_v1_bundle(
            backend.as_ref(),
            &released_v1_contents(1, 1, "released-branch-a"),
        )
        .await;
        publish_released_v1_bundle(
            backend.as_ref(),
            &released_v1_contents(1, 1, "released-branch-b"),
        )
        .await;
        publish_released_v1_bundle(
            backend.as_ref(),
            &released_v1_contents(3, 3, "released-head"),
        )
        .await;
        initialize_released_v1_test_vault(backend.as_ref()).await;
        let (store, pool) = test_store("device-upgraded").await;
        import_sessions(
            &pool,
            &[normalized_session(99, "local-must-not-publish", "local")],
            true,
        )
        .await
        .unwrap();
        let engine = SyncEngine::new(
            store.clone(),
            backend.clone(),
            "default",
            "generation-1",
            "device-upgraded",
        );

        let error = engine.run_once(SyncTrigger::Manual).await.unwrap_err();

        assert!(
            matches!(error, AppError::SyncProtocol(ref message) if message.contains("same sequence")),
            "{error:?}"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM sessions WHERE platform_session_id IN ('remote-1', 'remote-3')"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
        assert_eq!(store.pending_mutations(10).await.unwrap().len(), 1);
        assert!(
            backend
                .get(
                    &RemotePath::parse(
                        "v1/generations/generation-1/devices/device-upgraded/head.json"
                    )
                    .unwrap()
                )
                .await
                .is_err_and(|error| error.kind() == "not_found")
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sync_remote_cursors")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn persisted_cursor_anchor_rejects_current_chain_pointing_to_replaced_predecessor() {
        let server = TestWebDav::start("user", "pass").await;
        let backend = Arc::new(server.client("user", "pass").unwrap());
        let predecessor_a =
            publish_released_v1_bundle(backend.as_ref(), &released_v1_contents(1, 1, "accepted-a"))
                .await;
        initialize_released_v1_test_vault(backend.as_ref()).await;
        let (store, pool) = test_store("device-upgraded").await;
        let engine = SyncEngine::new(
            store.clone(),
            backend.clone(),
            "default",
            "generation-1",
            "device-upgraded",
        );
        assert_eq!(
            engine
                .pull_remote(
                    PullPolicy::StrictMaintenance,
                    Some(VaultCompatibility::ReleasedV1Writers),
                )
                .await
                .unwrap()
                .pulled,
            1
        );
        let accepted_title: String =
            sqlx::query_scalar("SELECT title FROM sessions WHERE platform_session_id = 'remote-1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(accepted_title, "accepted-a");
        let accepted_cursor = store
            .remote_cursor("generation-1", "device-old")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(accepted_cursor.cursor_seq, 1);
        assert_eq!(
            accepted_cursor.anchor.as_ref().map(|anchor| (
                anchor.end_seq,
                anchor.path.as_str(),
                anchor.sha256.as_str(),
            )),
            Some((
                1,
                predecessor_a.path.as_str(),
                predecessor_a.sha256.as_str(),
            ))
        );

        let predecessor_b = publish_released_v1_bundle(
            backend.as_ref(),
            &released_v1_contents(1, 1, "conflicting-b"),
        )
        .await;
        assert_ne!(predecessor_a.sha256, predecessor_b.sha256);
        let mut successor = released_v1_contents(2, 2, "must-not-apply");
        successor.previous_path = Some(predecessor_b.path.clone());
        successor.previous_sha256 = Some(predecessor_b.sha256.clone());
        successor.previous_end_seq = Some(predecessor_b.end_seq);
        publish_current_bundle(backend.as_ref(), &successor).await;

        let error = engine.run_once(SyncTrigger::Manual).await.unwrap_err();

        assert!(
            matches!(error, AppError::SyncProtocol(ref message) if message.contains("anchor")),
            "{error:?}"
        );
        let cursor = store
            .remote_cursor("generation-1", "device-old")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cursor.cursor_seq, 1);
        assert_eq!(cursor.anchor, accepted_cursor.anchor);
        let sessions: Vec<(String, String)> = sqlx::query_as(
            "SELECT platform_session_id, title FROM sessions ORDER BY platform_session_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(sessions, vec![("remote-1".into(), "accepted-a".into())]);
    }

    #[tokio::test]
    async fn released_v1_upgrade_keeps_pulling_old_unchained_continuations() {
        let server = TestWebDav::start("user", "pass").await;
        let backend = Arc::new(server.client("user", "pass").unwrap());
        publish_released_v1_bundle(
            backend.as_ref(),
            &released_v1_contents(1, 1, "released-one"),
        )
        .await;
        publish_released_v1_bundle(
            backend.as_ref(),
            &released_v1_contents(2, 2, "released-two"),
        )
        .await;
        initialize_released_v1_test_vault(backend.as_ref()).await;
        let (store, pool) = test_store("device-upgraded").await;
        let engine = SyncEngine::new(
            store,
            backend.clone(),
            "default",
            "generation-1",
            "device-upgraded",
        );

        let initial = engine.run_once(SyncTrigger::Manual).await.unwrap();
        assert_eq!(initial.pulled, 2);
        let initial_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(initial_count, 2);

        publish_released_v1_bundle(
            backend.as_ref(),
            &released_v1_contents(3, 3, "released-after-upgrade"),
        )
        .await;
        let continuation = engine.run_once(SyncTrigger::Manual).await.unwrap();
        assert_eq!(continuation.pulled, 1);
        let continued_title: String =
            sqlx::query_scalar("SELECT title FROM sessions WHERE platform_session_id = ?")
                .bind("remote-3")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(continued_title, "released-after-upgrade");
    }

    #[test]
    fn released_v1_compatibility_reader_does_not_weaken_strict_bundle_validation() {
        let sealed = seal_released_v1_unchained_bundle(&released_v1_contents(2, 2, "released-two"));

        let strict_error = open_bundle(&sealed.bytes, &BundleLimits::default()).unwrap_err();
        assert!(
            matches!(strict_error, AppError::InvalidData(ref message) if message.contains("previous bundle chain fields are invalid")),
            "{strict_error:?}"
        );
        let compatible = open_released_v1_unchained_bundle_protected(
            &sealed.bytes,
            &BundleLimits::default(),
            None,
        )
        .unwrap();
        assert_eq!(
            (compatible.header.start_seq, compatible.header.end_seq),
            (2, 2)
        );
        assert!(compatible.header.previous_path.is_none());
    }

    #[tokio::test]
    async fn released_v1_legacy_reconstruction_rejects_ambiguous_history() {
        let server = TestWebDav::start("user", "pass").await;
        let backend = Arc::new(server.client("user", "pass").unwrap());
        publish_released_v1_bundle(
            backend.as_ref(),
            &released_v1_contents(1, 1, "released-branch-a"),
        )
        .await;
        publish_released_v1_bundle(
            backend.as_ref(),
            &released_v1_contents(1, 4, "released-branch-b"),
        )
        .await;
        publish_released_v1_bundle(
            backend.as_ref(),
            &released_v1_contents(2, 2, "released-head"),
        )
        .await;
        initialize_released_v1_test_vault(backend.as_ref()).await;
        let (store, pool) = test_store("device-upgraded").await;
        let engine = SyncEngine::new(store, backend, "default", "generation-1", "device-upgraded");

        let error = engine
            .pull_remote(
                PullPolicy::StrictMaintenance,
                Some(VaultCompatibility::ReleasedV1Writers),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, AppError::SyncProtocol(ref message) if message.contains("history is ambiguous")),
            "{error:?}"
        );
        let session_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(session_count, 0);
    }

    #[test]
    fn scheduler_coalesces_by_priority_and_caps_backoff() {
        let mut state = SchedulerState::default();
        state.submit(SyncTrigger::Periodic);
        state.submit(SyncTrigger::Startup);
        state.submit(SyncTrigger::LocalMutation);
        state.submit(SyncTrigger::Manual);
        assert_eq!(state.take(), Some(SyncTrigger::Manual));
        assert_eq!(
            SchedulerState::delay_for(SyncTrigger::LocalMutation),
            Duration::from_secs(5)
        );
        assert_eq!(
            SchedulerState::delay_for(SyncTrigger::Periodic),
            Duration::from_secs(900)
        );
        for _ in 0..10 {
            state.failure(false);
        }
        assert_eq!(state.retry_delay, Duration::from_secs(3600));
        state.failure(true);
        assert!(state.paused_for_auth);
        state.submit(SyncTrigger::Manual);
        assert!(!state.paused_for_auth);
        state.success();
        assert_eq!(state.retry_delay, Duration::from_secs(30));
    }

    #[tokio::test]
    async fn publish_is_idempotent_and_acknowledges_outbox_after_head_update() {
        register_sqlite_vec();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        initialize_schema(&pool).await.unwrap();
        let store = SyncStore::new(pool.clone());
        store.initialize_device("device-a", "A").await.unwrap();
        let session = NormalizedSession {
            id: "local-1".into(),
            platform: "chat".into(),
            platform_session_id: "remote-1".into(),
            title: "title".into(),
            created_at: None,
            updated_at: None,
            imported_at: "2026-07-29T00:00:00Z".into(),
            messages: vec![],
            raw_data: json!({"fixture": true}),
        };
        import_sessions(&pool, &[session], true).await.unwrap();
        let server = TestWebDav::start("user", "pass").await;
        let backend = Arc::new(server.client("user", "pass").unwrap());
        initialize_test_vault(
            backend.as_ref(),
            "vault",
            "generation",
            VaultProtection::plain(),
        )
        .await;
        let engine = SyncEngine::new(store.clone(), backend, "vault", "generation", "device-a");
        let first = engine.run_once(SyncTrigger::Manual).await.unwrap();
        assert_eq!((first.published, first.acknowledged), (1, 1));
        assert!(store.pending_mutations(10).await.unwrap().is_empty());
        assert_eq!(
            engine.run_once(SyncTrigger::Manual).await.unwrap(),
            SyncReport::default()
        );
    }

    #[tokio::test]
    async fn writer_does_not_acknowledge_an_old_generation_after_it_is_frozen() {
        let server = TestS3::start("AKID", None).await;
        let inner = s3_backend(&server);
        let active = VaultDocument::active(
            VaultIdentity {
                format_version: 2,
                vault_id: "vault".into(),
                generation_id: "generation".into(),
            },
            VaultProtection::plain(),
        );
        load_or_create_vault(inner.as_ref(), active.clone())
            .await
            .unwrap();
        let backend = Arc::new(PauseBeforePublishVaultCasBackend::new(inner.clone()));
        let publish_attempted = backend.publish_attempted();
        let (store, _pool) = test_store("device-a").await;
        store
            .queue_local_upsert(snapshot(0, "late old-generation update"), 1_000)
            .await
            .unwrap();
        let engine = Arc::new(SyncEngine::new(
            store.clone(),
            backend.clone(),
            "vault",
            "generation",
            "device-a",
        ));
        let publishing_engine = engine.clone();
        let publishing = tokio::spawn(async move { publishing_engine.publish_pending().await });
        publish_attempted.notified().await;
        begin_generation_freeze(
            inner.as_ref(),
            &active,
            "generation-next",
            VaultProtection::plain(),
            "writer-fence",
        )
        .await
        .unwrap();
        backend.release();

        assert!(publishing.await.unwrap().is_err());
        assert_eq!(store.pending_mutation_count().await.unwrap(), 1);
        assert!(
            inner.get(&engine.head_path().unwrap()).await.is_err(),
            "a writer fenced by generation freeze must not advance the old head"
        );
    }

    #[tokio::test]
    async fn generation_rotation_captures_a_remote_writer_before_freeze_and_replays_exactly() {
        let server = TestS3::start("AKID", None).await;
        let shared_backend = s3_backend(&server);
        let old_identity = VaultIdentity {
            format_version: 2,
            vault_id: "vault".into(),
            generation_id: "generation".into(),
        };
        load_or_create_vault(
            shared_backend.as_ref(),
            VaultDocument::active(old_identity.clone(), VaultProtection::plain()),
        )
        .await
        .unwrap();

        let (store_a, _pool_a) = test_store("device-a").await;
        store_a
            .queue_local_upsert(snapshot(0, "base"), 1_000)
            .await
            .unwrap();
        let engine_a = SyncEngine::new(
            store_a.clone(),
            shared_backend.clone(),
            "vault",
            "generation",
            "device-a",
        );
        engine_a.run_once(SyncTrigger::Manual).await.unwrap();
        let old_a_head = engine_a.head_path().unwrap();

        let (store_b, pool_b) = test_store("device-b").await;
        let engine_b_old = SyncEngine::new(
            store_b.clone(),
            shared_backend.clone(),
            "vault",
            "generation",
            "device-b",
        );
        engine_b_old.run_once(SyncTrigger::Manual).await.unwrap();
        import_sessions(&pool_b, &[normalized_session(1, "from-b", "b")], true)
            .await
            .unwrap();
        store_b
            .queue_local_delete(
                EntityKey {
                    platform: "chat".into(),
                    platform_session_id: "remote-0".into(),
                },
                2_001,
            )
            .await
            .unwrap();

        let paused_backend = Arc::new(PauseBeforeVaultFreezeBackend::new(shared_backend.clone()));
        let freeze_attempted = paused_backend.freeze_attempted();
        let rotating = SyncEngine::new(
            store_a.clone(),
            paused_backend.clone(),
            "vault",
            "generation",
            "device-a",
        );
        let rotation = tokio::spawn(async move {
            rotating
                .rotate_generation("generation-next", VaultProtection::plain(), None)
                .await
        });
        freeze_attempted.notified().await;

        let before_release = load_versioned_identity(shared_backend.as_ref())
            .await
            .unwrap();
        assert_eq!(before_release.identity, old_identity);
        assert_eq!(before_release.state, VaultState::Active);

        let b_report = engine_b_old.run_once(SyncTrigger::Manual).await.unwrap();
        assert_eq!((b_report.published, b_report.acknowledged), (2, 2));
        let old_b_head = engine_b_old.head_path().unwrap();
        let b_versions: Vec<EntityVersionRow> = sqlx::query_as(
            "SELECT platform, platform_session_id, operation, version_wall_ms,
                    version_counter, version_device_id, content_hash
             FROM sync_entity_versions
             ORDER BY platform, platform_session_id",
        )
        .fetch_all(&pool_b)
        .await
        .unwrap();
        assert_eq!(b_versions.len(), 2);
        assert!(b_versions.iter().any(|row| row.2 == "delete"));

        paused_backend.release();
        let rotation_report = rotation.await.unwrap().unwrap();
        assert!(rotation_report.pulled >= 1);
        assert_eq!(rotation_report.published, 2);
        let active = load_versioned_identity(shared_backend.as_ref())
            .await
            .unwrap();
        assert_eq!(active.identity.generation_id, "generation-next");
        assert_eq!(active.state, VaultState::Active);

        assert!(shared_backend.get(&old_a_head).await.is_ok());
        assert!(shared_backend.get(&old_b_head).await.is_ok());

        let (store_c, pool_c) = test_store("device-c").await;
        let engine_c = SyncEngine::new(
            store_c,
            shared_backend.clone(),
            "vault",
            "generation-next",
            "device-c",
        );
        engine_c.run_once(SyncTrigger::Manual).await.unwrap();
        let c_title: String =
            sqlx::query_scalar("SELECT title FROM sessions WHERE platform_session_id = 'remote-1'")
                .fetch_one(&pool_c)
                .await
                .unwrap();
        assert_eq!(c_title, "from-b");
        let deleted_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sessions WHERE platform_session_id = 'remote-0'",
        )
        .fetch_one(&pool_c)
        .await
        .unwrap();
        assert_eq!(deleted_count, 0);
        let c_versions: Vec<EntityVersionRow> = sqlx::query_as(
            "SELECT platform, platform_session_id, operation, version_wall_ms,
                    version_counter, version_device_id, content_hash
             FROM sync_entity_versions
             ORDER BY platform, platform_session_id",
        )
        .fetch_all(&pool_c)
        .await
        .unwrap();
        assert_eq!(c_versions, b_versions);

        let b_next = SyncEngine::new(
            store_b.clone(),
            shared_backend.clone(),
            "vault",
            "generation-next",
            "device-b",
        );
        let adoption = b_next
            .run_once_with_generation_replay(SyncTrigger::Manual)
            .await
            .unwrap();
        assert_eq!(adoption.published, 2);
        assert_eq!(adoption.acknowledged, 2);
        assert!(store_b.pending_mutations(10).await.unwrap().is_empty());
        let marker: (String, String) = sqlx::query_as(
            "SELECT vault_id, generation_id FROM sync_publication_state WHERE singleton = 1",
        )
        .fetch_one(&pool_b)
        .await
        .unwrap();
        assert_eq!(marker, ("vault".into(), "generation-next".into()));

        let state_after_adoption = store_b.device_state().await.unwrap().unwrap();
        let repeated = b_next
            .run_once_with_generation_replay(SyncTrigger::Manual)
            .await
            .unwrap();
        assert_eq!(repeated, SyncReport::default());
        let state_after_repeat = store_b.device_state().await.unwrap().unwrap();
        assert_eq!(
            (
                state_after_repeat.hlc_wall_ms,
                state_after_repeat.hlc_counter,
                state_after_repeat.next_seq,
            ),
            (
                state_after_adoption.hlc_wall_ms,
                state_after_adoption.hlc_counter,
                state_after_adoption.next_seq,
            )
        );
    }

    #[tokio::test]
    async fn two_devices_pull_remote_bundle_without_echo_outbox() {
        register_sqlite_vec();
        let pool_a = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let pool_b = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        initialize_schema(&pool_a).await.unwrap();
        initialize_schema(&pool_b).await.unwrap();
        let store_a = SyncStore::new(pool_a.clone());
        let store_b = SyncStore::new(pool_b.clone());
        store_a.initialize_device("device-a", "A").await.unwrap();
        store_b.initialize_device("device-b", "B").await.unwrap();
        let session = NormalizedSession {
            id: "local-a".into(),
            platform: "chat".into(),
            platform_session_id: "remote-1".into(),
            title: "from-a".into(),
            created_at: None,
            updated_at: None,
            imported_at: "2026-07-29T00:00:00Z".into(),
            messages: vec![],
            raw_data: json!({"fixture": true}),
        };
        import_sessions(&pool_a, &[session], true).await.unwrap();
        let server = TestWebDav::start("user", "pass").await;
        let backend_a = Arc::new(server.client("user", "pass").unwrap());
        let backend_b = Arc::new(server.client("user", "pass").unwrap());
        initialize_test_vault(
            backend_a.as_ref(),
            "vault",
            "generation",
            VaultProtection::plain(),
        )
        .await;
        let engine_a = SyncEngine::new(store_a, backend_a, "vault", "generation", "device-a");
        let engine_b = SyncEngine::new(
            store_b.clone(),
            backend_b.clone(),
            "vault",
            "generation",
            "device-b",
        );
        let first_report = engine_a.run_once(SyncTrigger::Manual).await.unwrap();
        assert_eq!((first_report.published, first_report.acknowledged), (1, 1));
        let report = engine_b.run_once(SyncTrigger::Manual).await.unwrap();
        assert!(report.pulled >= 1);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(&pool_b)
            .await
            .unwrap();
        assert_eq!(count, 1);
        assert!(store_b.pending_mutations(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn s3_two_devices_converge_across_first_join_lww_delete_offline_recovery_and_retries() {
        let (store_a, pool_a) = test_store("device-a").await;
        let (store_b, pool_b) = test_store("device-b").await;
        let server = TestS3::start("AKID", None).await;
        initialize_test_vault(
            s3_backend(&server).as_ref(),
            "vault",
            "generation",
            VaultProtection::plain(),
        )
        .await;
        let engine_a = SyncEngine::new(
            store_a.clone(),
            s3_backend(&server),
            "vault",
            "generation",
            "device-a",
        );
        let engine_b = SyncEngine::new(
            store_b.clone(),
            s3_backend(&server),
            "vault",
            "generation",
            "device-b",
        );

        import_sessions(&pool_a, &[normalized_session(0, "only-a", "a")], true)
            .await
            .unwrap();
        import_sessions(&pool_b, &[normalized_session(1, "only-b", "b")], true)
            .await
            .unwrap();
        engine_a.run_once(SyncTrigger::Manual).await.unwrap();
        engine_b.run_once(SyncTrigger::Manual).await.unwrap();
        engine_a.run_once(SyncTrigger::Manual).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sessions")
                .fetch_one(&pool_a)
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sessions")
                .fetch_one(&pool_b)
                .await
                .unwrap(),
            2
        );

        import_sessions(&pool_a, &[normalized_session(0, "a-concurrent", "a")], true)
            .await
            .unwrap();
        import_sessions(&pool_b, &[normalized_session(0, "b-wins", "b")], true)
            .await
            .unwrap();
        engine_a.run_once(SyncTrigger::Manual).await.unwrap();
        engine_b.run_once(SyncTrigger::Manual).await.unwrap();
        engine_a.run_once(SyncTrigger::Manual).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT title FROM sessions WHERE platform_session_id = 'remote-0'",
            )
            .fetch_one(&pool_a)
            .await
            .unwrap(),
            "b-wins"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT title FROM sessions WHERE platform_session_id = 'remote-0'",
            )
            .fetch_one(&pool_b)
            .await
            .unwrap(),
            "b-wins"
        );

        store_a
            .queue_local_delete(
                EntityKey {
                    platform: "chat".into(),
                    platform_session_id: "remote-1".into(),
                },
                3_000,
            )
            .await
            .unwrap();
        sqlx::query("DELETE FROM sessions WHERE platform_session_id = 'remote-1'")
            .execute(&pool_a)
            .await
            .unwrap();
        engine_a.run_once(SyncTrigger::Manual).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM sessions WHERE platform_session_id = 'remote-1'",
            )
            .fetch_one(&pool_b)
            .await
            .unwrap(),
            1,
            "device B is intentionally offline until the next run"
        );
        engine_b.run_once(SyncTrigger::Manual).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM sessions WHERE platform_session_id = 'remote-1'",
            )
            .fetch_one(&pool_b)
            .await
            .unwrap(),
            0
        );

        assert_eq!(
            engine_a.run_once(SyncTrigger::Manual).await.unwrap(),
            SyncReport::default()
        );
        assert_eq!(
            engine_b.run_once(SyncTrigger::Manual).await.unwrap(),
            SyncReport::default()
        );
        let outbox_a: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_mutations")
            .fetch_one(&pool_a)
            .await
            .unwrap();
        let outbox_b: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_mutations")
            .fetch_one(&pool_b)
            .await
            .unwrap();
        assert_eq!((outbox_a, outbox_b), (0, 0));

        let cursors_a: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT generation_id, remote_device_id, cursor_seq
             FROM sync_remote_cursors ORDER BY generation_id, remote_device_id",
        )
        .fetch_all(&pool_a)
        .await
        .unwrap();
        let cursors_b: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT generation_id, remote_device_id, cursor_seq
             FROM sync_remote_cursors ORDER BY generation_id, remote_device_id",
        )
        .fetch_all(&pool_b)
        .await
        .unwrap();
        assert_eq!(cursors_a, vec![("generation".into(), "device-b".into(), 2)]);
        assert_eq!(cursors_b, vec![("generation".into(), "device-a".into(), 3)]);

        let versions_a: Vec<(String, String, String, String)> = sqlx::query_as(
            "SELECT platform, platform_session_id, operation, version_device_id
             FROM sync_entity_versions ORDER BY platform, platform_session_id",
        )
        .fetch_all(&pool_a)
        .await
        .unwrap();
        let versions_b: Vec<(String, String, String, String)> = sqlx::query_as(
            "SELECT platform, platform_session_id, operation, version_device_id
             FROM sync_entity_versions ORDER BY platform, platform_session_id",
        )
        .fetch_all(&pool_b)
        .await
        .unwrap();
        let expected_versions = vec![
            (
                "chat".into(),
                "remote-0".into(),
                "upsert".into(),
                "device-b".into(),
            ),
            (
                "chat".into(),
                "remote-1".into(),
                "delete".into(),
                "device-a".into(),
            ),
        ];
        assert_eq!(versions_a, expected_versions);
        assert_eq!(versions_b, expected_versions);

        let sessions_a: Vec<(String, String, String, String, String, String, String)> =
            sqlx::query_as(
                "SELECT platform, platform_session_id, COALESCE(title, ''),
                        COALESCE(created_at, ''), COALESCE(updated_at, ''),
                        COALESCE(imported_at, ''), COALESCE(raw_data, '')
                 FROM sessions ORDER BY platform, platform_session_id",
            )
            .fetch_all(&pool_a)
            .await
            .unwrap();
        let sessions_b: Vec<(String, String, String, String, String, String, String)> =
            sqlx::query_as(
                "SELECT platform, platform_session_id, COALESCE(title, ''),
                        COALESCE(created_at, ''), COALESCE(updated_at, ''),
                        COALESCE(imported_at, ''), COALESCE(raw_data, '')
                 FROM sessions ORDER BY platform, platform_session_id",
            )
            .fetch_all(&pool_b)
            .await
            .unwrap();
        assert_eq!(sessions_a, sessions_b);
        assert_eq!(
            sessions_a,
            vec![(
                "chat".into(),
                "remote-0".into(),
                "b-wins".into(),
                "".into(),
                "".into(),
                "2026-07-29T00:00:00Z".into(),
                r#"{"fixture":0}"#.into(),
            )]
        );
        let messages_a: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
            .fetch_one(&pool_a)
            .await
            .unwrap();
        let messages_b: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
            .fetch_one(&pool_b)
            .await
            .unwrap();
        assert_eq!((messages_a, messages_b), (0, 0));
    }

    #[tokio::test]
    async fn engine_accepts_a_dynamic_cloud_backend() {
        register_sqlite_vec();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        initialize_schema(&pool).await.unwrap();
        let store = SyncStore::new(pool);
        store.initialize_device("device-a", "A").await.unwrap();
        let server = TestWebDav::start("user", "pass").await;
        let backend: Arc<dyn CloudBackend> = Arc::new(server.client("user", "pass").unwrap());
        initialize_test_vault(
            backend.as_ref(),
            "vault",
            "generation",
            VaultProtection::plain(),
        )
        .await;

        let engine = SyncEngine::new(store, backend, "vault", "generation", "device-a");

        assert_eq!(
            engine.run_once(SyncTrigger::Manual).await.unwrap(),
            SyncReport::default()
        );
    }

    #[tokio::test]
    async fn protected_engine_uploads_ciphertext_and_same_protector_converges() {
        let (store_a, _pool_a) = test_store("device-a").await;
        let (store_b, pool_b) = test_store("device-b").await;
        store_a
            .queue_local_upsert(snapshot(0, "encrypted-source"), 1_000)
            .await
            .unwrap();
        let server = TestS3::start("AKID", None).await;
        let backend_a = s3_backend(&server);
        initialize_test_vault(
            backend_a.as_ref(),
            "vault",
            "generation",
            test_protection("shared passphrase"),
        )
        .await;
        let protector = test_protector("shared passphrase");
        let engine_a = SyncEngine::new_protected(
            store_a,
            backend_a.clone(),
            "vault",
            "generation",
            "device-a",
            Some(protector.clone()),
        );
        let engine_b = SyncEngine::new_protected(
            store_b,
            s3_backend(&server),
            "vault",
            "generation",
            "device-b",
            Some(protector),
        );

        engine_a.run_once(SyncTrigger::Manual).await.unwrap();
        let head: HeadDocument = serde_json::from_slice(
            &backend_a
                .get(&engine_a.head_path().unwrap())
                .await
                .unwrap()
                .bytes,
        )
        .unwrap();
        let uploaded = backend_a
            .get(&RemotePath::parse(&head.path).unwrap())
            .await
            .unwrap();
        assert!(open_bundle(&uploaded.bytes, &BundleLimits::default()).is_err());

        assert_eq!(
            engine_b.run_once(SyncTrigger::Manual).await.unwrap().pulled,
            1
        );
        let title: String = sqlx::query_scalar("SELECT title FROM sessions")
            .fetch_one(&pool_b)
            .await
            .unwrap();
        assert_eq!(title, "encrypted-source");
    }

    #[tokio::test]
    async fn encrypted_generation_rejects_a_plain_bundle_before_merge() {
        let (store, pool) = test_store("device-b").await;
        let server = TestS3::start("AKID", None).await;
        let backend = s3_backend(&server);
        let protection = test_protection("shared passphrase");
        initialize_test_vault(backend.as_ref(), "vault", "generation", protection.clone()).await;

        let snapshot = snapshot(0, "unauthenticated plain injection");
        let content_hash = sha256_hex(&serde_json::to_vec(&snapshot).unwrap());
        let contents = BundleContents {
            vault_id: "vault".into(),
            generation_id: "generation".into(),
            device_id: "attacker".into(),
            start_seq: 1,
            end_seq: 1,
            previous_path: None,
            previous_sha256: None,
            previous_end_seq: None,
            changes: vec![BundleChange {
                local_seq: 1,
                key: snapshot.key.clone(),
                operation: MutationOperation::Upsert,
                version: EntityVersion::new(i64::MAX - 1, 0, "attacker"),
                content_hash: Some(content_hash),
                snapshot: Some(snapshot),
            }],
        };
        let sealed = seal_bundle(&contents).unwrap();
        let bundle_path = RemotePath::parse(&format!(
            "v1/generations/generation/devices/attacker/bundles/1-1-{}.acmb",
            sealed.file_sha256
        ))
        .unwrap();
        backend
            .put_immutable(&bundle_path, &sealed.bytes)
            .await
            .unwrap();
        let head = HeadDocument {
            generation_id: "generation".into(),
            device_id: "attacker".into(),
            end_seq: 1,
            path: bundle_path.display(),
            sha256: sealed.file_sha256,
        };
        backend
            .put_if_absent(
                &RemotePath::parse("v1/generations/generation/devices/attacker/head.json").unwrap(),
                &serde_json::to_vec(&head).unwrap(),
            )
            .await
            .unwrap();

        let engine = SyncEngine::new_protected_with_policy(
            store,
            backend,
            "vault",
            "generation",
            "device-b",
            protection,
            Some(test_protector("shared passphrase")),
        );
        let error = engine.run_once(SyncTrigger::Manual).await.unwrap_err();

        assert!(matches!(error, AppError::Crypto(_)), "{error:?}");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn encrypted_generation_rejects_a_legacy_plain_staged_bundle() {
        let (store, _pool) = test_store("device-a").await;
        store
            .queue_local_upsert(snapshot(0, "plain staged payload"), 1_000)
            .await
            .unwrap();
        let server = TestS3::start("AKID", None).await;
        let backend = s3_backend(&server);
        let protection = test_protection("shared passphrase");
        initialize_test_vault(backend.as_ref(), "vault", "generation", protection.clone()).await;
        let engine = SyncEngine::new_protected_with_policy(
            store.clone(),
            backend.clone(),
            "vault",
            "generation",
            "device-a",
            protection,
            Some(test_protector("shared passphrase")),
        );
        let pending = store.pending_mutations(10).await.unwrap();
        let contents = engine.contents_from_pending(&pending, None).unwrap();
        let sealed = seal_bundle(&contents).unwrap();
        let path = engine
            .bundle_path(&sealed, contents.start_seq, contents.end_seq)
            .unwrap();
        store
            .stage_bundle(
                &sealed.file_sha256,
                "generation",
                "device-a",
                &path.display(),
                contents.start_seq,
                contents.end_seq,
                &sealed.bytes,
                current_time_millis(),
            )
            .await
            .unwrap();

        let error = engine.publish_pending().await.unwrap_err();

        assert!(matches!(error, AppError::Crypto(_)), "{error:?}");
        assert_eq!(
            backend
                .get(&engine.head_path().unwrap())
                .await
                .unwrap_err()
                .kind(),
            "not_found"
        );
        assert_eq!(store.pending_mutations(10).await.unwrap(), pending);
    }

    #[tokio::test]
    async fn staged_bundle_must_match_the_current_outbox_prefix() {
        let (store, _pool) = test_store("device-a").await;
        store
            .queue_local_upsert(snapshot(0, "current local mutation"), 1_000)
            .await
            .unwrap();
        let server = TestS3::start("AKID", None).await;
        let backend = s3_backend(&server);
        initialize_test_vault(
            backend.as_ref(),
            "vault",
            "generation",
            VaultProtection::plain(),
        )
        .await;
        let engine = SyncEngine::new(
            store.clone(),
            backend.clone(),
            "vault",
            "generation",
            "device-a",
        );
        let pending = store.pending_mutations(10).await.unwrap();
        let mut mismatched = pending.clone();
        let snapshot = mismatched[0].snapshot.as_mut().unwrap();
        snapshot.title = "different staged mutation".into();
        mismatched[0].content_hash = Some(sha256_hex(&serde_json::to_vec(snapshot).unwrap()));
        let contents = engine.contents_from_pending(&mismatched, None).unwrap();
        let sealed = seal_bundle(&contents).unwrap();
        let path = engine
            .bundle_path(&sealed, contents.start_seq, contents.end_seq)
            .unwrap();
        store
            .stage_bundle(
                &sealed.file_sha256,
                "generation",
                "device-a",
                &path.display(),
                contents.start_seq,
                contents.end_seq,
                &sealed.bytes,
                current_time_millis(),
            )
            .await
            .unwrap();

        let error = engine.publish_pending().await.unwrap_err();

        assert!(
            matches!(error, AppError::InvalidData(ref message) if message.contains("current outbox prefix")),
            "{error:?}"
        );
        assert_eq!(store.pending_mutations(10).await.unwrap(), pending);
        assert_eq!(
            backend
                .get(&engine.head_path().unwrap())
                .await
                .unwrap_err()
                .kind(),
            "not_found"
        );
    }

    #[tokio::test]
    async fn publish_splits_by_final_envelope_bytes_and_keeps_each_bundle_readable() {
        let (store, _pool) = test_store("device-a").await;
        store
            .queue_local_upsert(noisy_snapshot(0, "first size-aware mutation"), 1_000)
            .await
            .unwrap();
        store
            .queue_local_upsert(noisy_snapshot(1, "second size-aware mutation"), 1_001)
            .await
            .unwrap();
        let server = TestS3::start("AKID", None).await;
        let backend = s3_backend(&server);
        initialize_test_vault(
            backend.as_ref(),
            "vault",
            "generation",
            VaultProtection::plain(),
        )
        .await;
        let probe = SyncEngine::new(
            store.clone(),
            backend.clone(),
            "vault",
            "generation",
            "device-a",
        );
        let pending = store.pending_mutations(10).await.unwrap();
        let one = seal_bundle(&probe.contents_from_pending(&pending[..1], None).unwrap()).unwrap();
        let two = seal_bundle(&probe.contents_from_pending(&pending, None).unwrap()).unwrap();
        let first_path = probe
            .bundle_path(&one, one.header.start_seq, one.header.end_seq)
            .unwrap();
        let first_head = HeadDocument {
            generation_id: "generation".into(),
            device_id: "device-a".into(),
            end_seq: one.header.end_seq,
            path: first_path.display(),
            sha256: one.file_sha256.clone(),
        };
        let second = seal_bundle(
            &probe
                .contents_from_pending(&pending[1..], Some(&first_head))
                .unwrap(),
        )
        .unwrap();
        let largest_single = one.bytes.len().max(second.bytes.len());
        assert!(two.bytes.len() > largest_single);
        let limits = BundleLimits {
            max_envelope_bytes: largest_single,
            ..BundleLimits::default()
        };
        let max_envelope_bytes = limits.max_envelope_bytes;
        let engine = probe.with_bundle_limits(limits);

        let report = engine.run_once(SyncTrigger::Manual).await.unwrap();

        assert_eq!((report.published, report.acknowledged), (2, 2));
        let head: HeadDocument = serde_json::from_slice(
            &backend
                .get(&engine.head_path().unwrap())
                .await
                .unwrap()
                .bytes,
        )
        .unwrap();
        let final_bytes = backend
            .get(&RemotePath::parse(&head.path).unwrap())
            .await
            .unwrap()
            .bytes;
        assert!(final_bytes.len() <= max_envelope_bytes);
        let final_bundle = open_bundle(&final_bytes, &engine.bundle_limits).unwrap();
        assert_eq!(
            (final_bundle.header.start_seq, final_bundle.header.end_seq),
            (2, 2)
        );
        let first_bytes = backend
            .get(&RemotePath::parse(final_bundle.header.previous_path.as_deref().unwrap()).unwrap())
            .await
            .unwrap()
            .bytes;
        assert!(first_bytes.len() <= max_envelope_bytes);
        let first_bundle = open_bundle(&first_bytes, &engine.bundle_limits).unwrap();
        assert_eq!(
            (first_bundle.header.start_seq, first_bundle.header.end_seq),
            (1, 1)
        );
    }

    #[tokio::test]
    async fn single_mutation_over_bundle_limit_is_not_staged_or_uploaded() {
        let (store, pool) = test_store("device-a").await;
        store
            .queue_local_upsert(snapshot(0, "single oversized mutation"), 1_000)
            .await
            .unwrap();
        let server = TestS3::start("AKID", None).await;
        let backend = s3_backend(&server);
        initialize_test_vault(
            backend.as_ref(),
            "vault",
            "generation",
            VaultProtection::plain(),
        )
        .await;
        let probe = SyncEngine::new(
            store.clone(),
            backend.clone(),
            "vault",
            "generation",
            "device-a",
        );
        let pending = store.pending_mutations(10).await.unwrap();
        let sealed = seal_bundle(&probe.contents_from_pending(&pending, None).unwrap()).unwrap();
        let limits = BundleLimits {
            max_envelope_bytes: sealed.bytes.len() - 1,
            ..BundleLimits::default()
        };
        let engine = probe.with_bundle_limits(limits);

        let error = engine.publish_pending().await.unwrap_err();

        assert!(
            matches!(error, AppError::InvalidData(ref message) if message.contains("single mutation exceeds bundle limits")),
            "{error:?}"
        );
        assert_eq!(
            backend
                .get(&engine.head_path().unwrap())
                .await
                .unwrap_err()
                .kind(),
            "not_found"
        );
        let staged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_published_bundles")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(staged, 0);
    }

    #[tokio::test]
    async fn oversized_first_mutation_is_sealed_only_once_before_rejection() {
        struct CountingProtector {
            seal_calls: AtomicUsize,
        }

        impl PayloadProtector for CountingProtector {
            fn algorithm(&self) -> ProtectionAlgorithm {
                ProtectionAlgorithm::XChaCha20Poly1305
            }

            fn seal(
                &self,
                _associated_data: &[u8],
                plaintext: &[u8],
                _nonce: [u8; 24],
            ) -> Result<Vec<u8>> {
                self.seal_calls.fetch_add(1, Ordering::SeqCst);
                Ok(plaintext.to_vec())
            }

            fn open(
                &self,
                _associated_data: &[u8],
                ciphertext: &[u8],
                _nonce: [u8; 24],
            ) -> Result<Vec<u8>> {
                Ok(ciphertext.to_vec())
            }
        }

        let (store, _pool) = test_store("device-a").await;
        let server = TestS3::start("AKID", None).await;
        let backend = s3_backend(&server);
        let protector = Arc::new(CountingProtector {
            seal_calls: AtomicUsize::new(0),
        });
        let engine = SyncEngine::new_protected(
            store,
            backend,
            "vault",
            "generation",
            "device-a",
            Some(protector.clone()),
        )
        .with_bundle_limits(BundleLimits {
            max_file_bytes: 1,
            ..BundleLimits::default()
        });
        let pending = (1..=MAX_MUTATIONS_PER_BUNDLE)
            .map(|local_seq| {
                let snapshot = snapshot(local_seq, "oversized");
                let content_hash = sha256_hex(&serde_json::to_vec(&snapshot).unwrap());
                PendingMutation {
                    key: snapshot.key.clone(),
                    local_seq: local_seq as i64,
                    operation: MutationOperation::Upsert,
                    version: EntityVersion {
                        wall_ms: 1_000 + local_seq as i64,
                        counter: 0,
                        device_id: "device-a".into(),
                    },
                    content_hash: Some(content_hash),
                    snapshot: Some(snapshot),
                }
            })
            .collect::<Vec<_>>();

        let error = engine
            .seal_largest_mutation_prefix(
                &pending,
                None,
                "generation",
                "device-a",
                Some(protector.as_ref()),
            )
            .unwrap_err();

        assert!(
            matches!(error, AppError::InvalidData(ref message) if message.contains("single mutation exceeds bundle limits")),
            "{error:?}"
        );
        assert_eq!(protector.seal_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn generation_baseline_splits_by_final_envelope_bytes() {
        let (store, pool) = test_store("device-a").await;
        import_sessions(
            &pool,
            &[
                noisy_normalized_session(0, "first baseline mutation"),
                noisy_normalized_session(1, "second baseline mutation"),
            ],
            true,
        )
        .await
        .unwrap();
        let server = TestS3::start("AKID", None).await;
        let backend = s3_backend(&server);
        initialize_test_vault(
            backend.as_ref(),
            "vault",
            "generation",
            VaultProtection::plain(),
        )
        .await;
        let probe = SyncEngine::new(
            store.clone(),
            backend.clone(),
            "vault",
            "generation",
            "device-a",
        );
        let baseline = store.baseline_mutations().await.unwrap();
        let first_contents = probe
            .contents_from_mutations_for(&baseline[..1], None, "generation-next", "baseline")
            .unwrap();
        let first = seal_bundle(&first_contents).unwrap();
        let first_path = RemotePath::parse(&format!(
            "v1/generations/generation-next/devices/baseline/bundles/{}-{}-{}.acmb",
            first_contents.start_seq, first_contents.end_seq, first.file_sha256
        ))
        .unwrap();
        let first_head = HeadDocument {
            generation_id: "generation-next".into(),
            device_id: "baseline".into(),
            end_seq: first_contents.end_seq,
            path: first_path.display(),
            sha256: first.file_sha256.clone(),
        };
        let second = seal_bundle(
            &probe
                .contents_from_mutations_for(
                    &baseline[1..],
                    Some(&first_head),
                    "generation-next",
                    "baseline",
                )
                .unwrap(),
        )
        .unwrap();
        let combined = seal_bundle(
            &probe
                .contents_from_mutations_for(&baseline, None, "generation-next", "baseline")
                .unwrap(),
        )
        .unwrap();
        let largest_single = first.bytes.len().max(second.bytes.len());
        assert!(combined.bytes.len() > largest_single);
        let limits = BundleLimits {
            max_envelope_bytes: largest_single,
            ..BundleLimits::default()
        };
        let engine = probe.with_bundle_limits(limits);

        let report = engine
            .rotate_generation("generation-next", VaultProtection::plain(), None)
            .await
            .unwrap();

        assert_eq!(report.published, 2);
        let vault = load_versioned_identity(backend.as_ref()).await.unwrap();
        assert_eq!(vault.identity.generation_id, "generation-next");
        let head_path =
            RemotePath::parse("v1/generations/generation-next/devices/baseline/head.json").unwrap();
        let head: HeadDocument =
            serde_json::from_slice(&backend.get(&head_path).await.unwrap().bytes).unwrap();
        let final_bytes = backend
            .get(&RemotePath::parse(&head.path).unwrap())
            .await
            .unwrap()
            .bytes;
        let final_bundle = open_bundle(&final_bytes, &engine.bundle_limits).unwrap();
        assert_eq!(
            (final_bundle.header.start_seq, final_bundle.header.end_seq),
            (2, 2)
        );
        let first_bytes = backend
            .get(&RemotePath::parse(final_bundle.header.previous_path.as_deref().unwrap()).unwrap())
            .await
            .unwrap()
            .bytes;
        let first_bundle = open_bundle(&first_bytes, &engine.bundle_limits).unwrap();
        assert_eq!(
            (first_bundle.header.start_seq, first_bundle.header.end_seq),
            (1, 1)
        );
    }

    #[tokio::test]
    async fn oversized_single_baseline_rolls_back_without_a_new_head() {
        let (store, pool) = test_store("device-a").await;
        import_sessions(
            &pool,
            &[noisy_normalized_session(0, "oversized baseline mutation")],
            true,
        )
        .await
        .unwrap();
        let server = TestS3::start("AKID", None).await;
        let backend = s3_backend(&server);
        initialize_test_vault(
            backend.as_ref(),
            "vault",
            "generation",
            VaultProtection::plain(),
        )
        .await;
        let probe = SyncEngine::new(
            store.clone(),
            backend.clone(),
            "vault",
            "generation",
            "device-a",
        );
        let baseline = store.baseline_mutations().await.unwrap();
        let sealed = seal_bundle(
            &probe
                .contents_from_mutations_for(&baseline, None, "generation-next", "baseline")
                .unwrap(),
        )
        .unwrap();
        let limits = BundleLimits {
            max_envelope_bytes: sealed.bytes.len() - 1,
            ..BundleLimits::default()
        };
        let engine = probe.with_bundle_limits(limits);

        let error = engine
            .rotate_generation("generation-next", VaultProtection::plain(), None)
            .await
            .unwrap_err();

        assert!(
            matches!(error, AppError::InvalidData(ref message) if message.contains("single mutation exceeds bundle limits")),
            "{error:?}"
        );
        let vault = load_versioned_identity(backend.as_ref()).await.unwrap();
        assert_eq!(vault.identity.generation_id, "generation");
        assert_eq!(vault.state, VaultState::Active);
        let new_head =
            RemotePath::parse("v1/generations/generation-next/devices/baseline/head.json").unwrap();
        assert_eq!(
            backend.get(&new_head).await.unwrap_err().kind(),
            "not_found"
        );
        let bundle_root =
            RemotePath::parse("v1/generations/generation-next/devices/baseline/bundles").unwrap();
        assert!(
            backend
                .list_depth_one(&bundle_root)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn protected_publish_reuses_staged_bytes_after_head_failure_and_restart() {
        let database_path = std::env::temp_dir().join(format!(
            "sync-encrypted-staging-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let pool = crate::database::connect(&database_path).await.unwrap();
        let store = SyncStore::new(pool.clone());
        store
            .initialize_device("device-a", "device-a")
            .await
            .unwrap();
        store
            .queue_local_upsert(snapshot(0, "encrypted-retry"), 1_000)
            .await
            .unwrap();

        let server = TestS3::start("AKID", None).await;
        let backend = Arc::new(FailFirstHeadWriteBackend::new(s3_backend(&server)));
        initialize_test_vault(
            backend.as_ref(),
            "vault",
            "generation",
            test_protection("shared passphrase"),
        )
        .await;
        let first_engine = SyncEngine::new_protected(
            store.clone(),
            backend.clone(),
            "vault",
            "generation",
            "device-a",
            Some(test_protector("shared passphrase")),
        );

        let error = first_engine.publish_pending().await.unwrap_err();
        assert!(matches!(error, AppError::Cloud(_)), "{error:?}");
        let first_objects = backend.bundle_objects().await;
        assert_eq!(first_objects.len(), 1);
        let (first_path, first_bytes) = first_objects.into_iter().next().unwrap();
        let first_sha256 = sha256_hex(&first_bytes);
        assert!(first_path.ends_with(&format!("-{first_sha256}.acmb")));
        assert!(open_bundle(&first_bytes, &BundleLimits::default()).is_err());
        let staged: (String, String, String, i64, i64, Vec<u8>) = sqlx::query_as(
            "SELECT generation_id, device_id, object_path, start_seq, end_seq, bundle_bytes
             FROM sync_published_bundles WHERE bundle_sha256 = ? AND stage = 'staged'",
        )
        .bind(&first_sha256)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(staged.0, "generation");
        assert_eq!(staged.1, "device-a");
        assert_eq!(staged.2, first_path);
        assert_eq!((staged.3, staged.4), (1, 1));
        assert_eq!(staged.5, first_bytes);

        drop(first_engine);
        drop(store);
        pool.close().await;

        let reopened_pool = crate::database::connect(&database_path).await.unwrap();
        let reopened_store = SyncStore::new(reopened_pool.clone());
        let restarted_engine = SyncEngine::new_protected(
            reopened_store.clone(),
            backend.clone(),
            "vault",
            "generation",
            "device-a",
            Some(test_protector("shared passphrase")),
        );

        let retry = restarted_engine.publish_pending().await.unwrap();
        assert_eq!((retry.published, retry.acknowledged), (1, 1));
        let final_objects = backend.bundle_objects().await;
        assert_eq!(
            final_objects.len(),
            1,
            "retry created a second orphan bundle"
        );
        assert_eq!(final_objects.get(&first_path), Some(&first_bytes));

        let head: HeadDocument = serde_json::from_slice(
            &backend
                .get(&restarted_engine.head_path().unwrap())
                .await
                .unwrap()
                .bytes,
        )
        .unwrap();
        assert_eq!(head.path, first_path);
        assert_eq!(head.sha256, first_sha256);
        assert_eq!(
            backend
                .get(&RemotePath::parse(&head.path).unwrap())
                .await
                .unwrap()
                .bytes,
            first_bytes
        );
        assert!(
            reopened_store
                .pending_mutations(1)
                .await
                .unwrap()
                .is_empty()
        );
        let confirmed: (String, Option<Vec<u8>>) = sqlx::query_as(
            "SELECT stage, bundle_bytes FROM sync_published_bundles WHERE bundle_sha256 = ?",
        )
        .bind(&first_sha256)
        .fetch_one(&reopened_pool)
        .await
        .unwrap();
        assert_eq!(confirmed, ("published".into(), None));

        reopened_pool.close().await;
        let _ = tokio::fs::remove_file(&database_path).await;
    }

    #[tokio::test]
    async fn protected_publish_reuses_staged_prefix_when_outbox_grows_during_restart() {
        let (store, _pool) = test_store("device-a").await;
        store
            .queue_local_upsert(snapshot(0, "first"), 1_000)
            .await
            .unwrap();
        let server = TestS3::start("AKID", None).await;
        let backend = Arc::new(FailFirstHeadWriteBackend::new(s3_backend(&server)));
        initialize_test_vault(
            backend.as_ref(),
            "vault",
            "generation",
            test_protection("shared passphrase"),
        )
        .await;
        let first_engine = SyncEngine::new_protected(
            store.clone(),
            backend.clone(),
            "vault",
            "generation",
            "device-a",
            Some(test_protector("shared passphrase")),
        );

        first_engine.publish_pending().await.unwrap_err();
        let first_objects = backend.bundle_objects().await;
        let (first_path, first_bytes) = first_objects.into_iter().next().unwrap();
        store
            .queue_local_upsert(snapshot(1, "second"), 2_000)
            .await
            .unwrap();

        let restarted_engine = SyncEngine::new_protected(
            store.clone(),
            backend.clone(),
            "vault",
            "generation",
            "device-a",
            Some(test_protector("shared passphrase")),
        );
        let retry = restarted_engine.publish_pending().await.unwrap();

        assert_eq!((retry.published, retry.acknowledged), (1, 1));
        assert_eq!(backend.bundle_objects().await.len(), 1);
        let head: HeadDocument = serde_json::from_slice(
            &backend
                .get(&restarted_engine.head_path().unwrap())
                .await
                .unwrap()
                .bytes,
        )
        .unwrap();
        assert_eq!(head.path, first_path);
        assert_eq!(
            backend
                .get(&RemotePath::parse(&head.path).unwrap())
                .await
                .unwrap()
                .bytes,
            first_bytes
        );
        let pending = store.pending_mutations(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].local_seq, 2);
    }

    #[tokio::test]
    async fn protected_publish_recovers_staged_bundle_after_outbox_coalescing() {
        let (store, _pool) = test_store("device-a").await;
        store
            .queue_local_upsert(snapshot(0, "first"), 1_000)
            .await
            .unwrap();
        let server = TestS3::start("AKID", None).await;
        let backend = Arc::new(FailFirstHeadWriteBackend::new(s3_backend(&server)));
        initialize_test_vault(
            backend.as_ref(),
            "vault",
            "generation",
            test_protection("shared passphrase"),
        )
        .await;
        let first_engine = SyncEngine::new_protected(
            store.clone(),
            backend.clone(),
            "vault",
            "generation",
            "device-a",
            Some(test_protector("shared passphrase")),
        );

        first_engine.publish_pending().await.unwrap_err();
        let first_objects = backend.bundle_objects().await;
        let (first_path, first_bytes) = first_objects.into_iter().next().unwrap();
        store
            .queue_local_upsert(snapshot(0, "newer"), 2_000)
            .await
            .unwrap();
        assert_eq!(store.pending_mutations(10).await.unwrap()[0].local_seq, 2);

        let restarted_engine = SyncEngine::new_protected(
            store.clone(),
            backend.clone(),
            "vault",
            "generation",
            "device-a",
            Some(test_protector("shared passphrase")),
        );
        let recovered = restarted_engine.publish_pending().await.unwrap();

        assert_eq!((recovered.published, recovered.acknowledged), (1, 0));
        assert_eq!(backend.bundle_objects().await.len(), 1);
        let first_head: HeadDocument = serde_json::from_slice(
            &backend
                .get(&restarted_engine.head_path().unwrap())
                .await
                .unwrap()
                .bytes,
        )
        .unwrap();
        assert_eq!(first_head.path, first_path);
        assert_eq!(
            backend
                .get(&RemotePath::parse(&first_head.path).unwrap())
                .await
                .unwrap()
                .bytes,
            first_bytes
        );

        let newer = restarted_engine.publish_pending().await.unwrap();
        assert_eq!((newer.published, newer.acknowledged), (1, 1));
        assert_eq!(backend.bundle_objects().await.len(), 2);
        assert!(store.pending_mutations(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn protected_bundle_maps_missing_and_wrong_protectors_to_crypto_errors() {
        let (store_a, _pool_a) = test_store("device-a").await;
        store_a
            .queue_local_upsert(snapshot(0, "encrypted-source"), 1_000)
            .await
            .unwrap();
        let server = TestS3::start("AKID", None).await;
        let backend = s3_backend(&server);
        initialize_test_vault(
            backend.as_ref(),
            "vault",
            "generation",
            test_protection("correct passphrase"),
        )
        .await;
        let publisher = SyncEngine::new_protected(
            store_a,
            backend.clone(),
            "vault",
            "generation",
            "device-a",
            Some(test_protector("correct passphrase")),
        );
        publisher.run_once(SyncTrigger::Manual).await.unwrap();

        let (missing_store, _missing_pool) = test_store("device-missing").await;
        let missing = SyncEngine::new(
            missing_store,
            backend.clone(),
            "vault",
            "generation",
            "device-missing",
        );
        assert!(matches!(
            missing.run_once(SyncTrigger::Manual).await,
            Err(AppError::InvalidData(message)) if message.contains("not active")
        ));

        let (wrong_store, _wrong_pool) = test_store("device-wrong").await;
        let wrong = SyncEngine::new_protected(
            wrong_store,
            backend,
            "vault",
            "generation",
            "device-wrong",
            Some(test_protector("wrong passphrase")),
        );
        assert!(matches!(
            wrong.run_once(SyncTrigger::Manual).await,
            Err(AppError::Crypto(message)) if message.contains("authentication failed")
        ));
    }

    #[tokio::test]
    async fn transient_s3_head_failure_aborts_pull_before_local_publish() {
        let (remote_store, _remote_pool) = test_store("device-remote").await;
        let (local_store, _local_pool) = test_store("device-local").await;
        remote_store
            .queue_local_upsert(snapshot(0, "remote-source"), 1_000)
            .await
            .unwrap();
        local_store
            .queue_local_upsert(snapshot(1, "local-pending"), 1_001)
            .await
            .unwrap();
        let server = TestS3::start("AKID", None).await;
        initialize_test_vault(
            s3_backend(&server).as_ref(),
            "vault",
            "generation",
            VaultProtection::plain(),
        )
        .await;
        SyncEngine::new(
            remote_store,
            s3_backend(&server),
            "vault",
            "generation",
            "device-remote",
        )
        .run_once(SyncTrigger::Manual)
        .await
        .unwrap();

        server
            .fail_next_get_with(axum::http::StatusCode::SERVICE_UNAVAILABLE)
            .await;
        let local = SyncEngine::new(
            local_store.clone(),
            s3_backend(&server),
            "vault",
            "generation",
            "device-local",
        );

        assert!(matches!(
            local.run_once(SyncTrigger::Manual).await,
            Err(AppError::Cloud(error)) if error.kind() == "offline"
        ));
        assert_eq!(local_store.pending_mutations(10).await.unwrap().len(), 1);
        server.clear_get_failure().await;
        assert_eq!(
            local
                .backend
                .get(&local.head_path().unwrap())
                .await
                .unwrap_err()
                .kind(),
            "not_found"
        );
    }

    #[tokio::test]
    async fn remote_bundle_from_another_vault_is_not_merged() {
        let (store_a, _pool_a) = test_store("device-a").await;
        let (store_b, pool_b) = test_store("device-b").await;
        store_a
            .queue_local_upsert(snapshot(0, "wrong-vault"), 1_000)
            .await
            .unwrap();
        let server = TestS3::start("AKID", None).await;
        initialize_test_vault(
            s3_backend(&server).as_ref(),
            "vault-a",
            "generation",
            VaultProtection::plain(),
        )
        .await;
        SyncEngine::new(
            store_a,
            s3_backend(&server),
            "vault-a",
            "generation",
            "device-a",
        )
        .run_once(SyncTrigger::Manual)
        .await
        .unwrap();
        let result = SyncEngine::new(
            store_b,
            s3_backend(&server),
            "vault-b",
            "generation",
            "device-b",
        )
        .run_once(SyncTrigger::Manual)
        .await;

        assert!(matches!(result, Err(AppError::InvalidData(_))));
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(&pool_b)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn s3_large_baseline_publishes_contiguous_bundle_chain_and_converges() {
        let (store_a, _pool_a) = test_store("device-a").await;
        let (store_b, pool_b) = test_store("device-b").await;
        for index in 0..501 {
            store_a
                .queue_local_upsert(
                    snapshot(index, &format!("from-a-{index}")),
                    1_000 + index as i64,
                )
                .await
                .unwrap();
        }
        let server = TestS3::start("AKID", None).await;
        initialize_test_vault(
            s3_backend(&server).as_ref(),
            "vault",
            "generation",
            VaultProtection::plain(),
        )
        .await;
        let backend_a = s3_backend(&server);
        let engine_a = SyncEngine::new(
            store_a.clone(),
            backend_a.clone(),
            "vault",
            "generation",
            "device-a",
        );
        let engine_b = SyncEngine::new(
            store_b,
            s3_backend(&server),
            "vault",
            "generation",
            "device-b",
        );

        let published = engine_a.run_once(SyncTrigger::Manual).await.unwrap();

        assert_eq!((published.published, published.acknowledged), (501, 501));
        assert!(store_a.pending_mutations(1).await.unwrap().is_empty());
        let head: HeadDocument = serde_json::from_slice(
            &backend_a
                .get(&engine_a.head_path().unwrap())
                .await
                .unwrap()
                .bytes,
        )
        .unwrap();
        assert_eq!(head.end_seq, 501);
        let final_bundle = backend_a
            .get(&RemotePath::parse(&head.path).unwrap())
            .await
            .unwrap()
            .bytes;
        let final_bundle = open_bundle(&final_bundle, &BundleLimits::default()).unwrap();
        assert_eq!(
            (final_bundle.header.start_seq, final_bundle.header.end_seq),
            (501, 501)
        );
        assert_eq!(final_bundle.header.previous_end_seq, Some(500));
        let first_path = final_bundle.header.previous_path.as_deref().unwrap();
        let first_bundle = backend_a
            .get(&RemotePath::parse(first_path).unwrap())
            .await
            .unwrap()
            .bytes;
        assert_eq!(
            final_bundle.header.previous_sha256.as_deref(),
            Some(sha256_hex(&first_bundle).as_str())
        );
        let first_bundle = open_bundle(&first_bundle, &BundleLimits::default()).unwrap();
        assert_eq!(
            (first_bundle.header.start_seq, first_bundle.header.end_seq),
            (1, 500)
        );
        assert_eq!(
            (
                first_bundle.header.previous_path,
                first_bundle.header.previous_sha256,
                first_bundle.header.previous_end_seq,
            ),
            (None, None, None)
        );

        let pulled = engine_b.run_once(SyncTrigger::Manual).await.unwrap();
        assert_eq!(pulled.pulled, 501);
        let session_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(&pool_b)
            .await
            .unwrap();
        assert_eq!(session_count, 501);
    }

    #[tokio::test]
    async fn coalesced_outbox_sequence_gap_still_converges() {
        let (store_a, _pool_a) = test_store("device-a").await;
        let (store_b, pool_b) = test_store("device-b").await;
        store_a
            .queue_local_upsert(snapshot(0, "first"), 1_000)
            .await
            .unwrap();
        store_a
            .queue_local_upsert(snapshot(0, "latest"), 1_001)
            .await
            .unwrap();
        let pending = store_a.pending_mutations(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].local_seq, 2);

        let server = TestS3::start("AKID", None).await;
        initialize_test_vault(
            s3_backend(&server).as_ref(),
            "vault",
            "generation",
            VaultProtection::plain(),
        )
        .await;
        let backend_a = s3_backend(&server);
        let engine_a = SyncEngine::new(
            store_a,
            backend_a.clone(),
            "vault",
            "generation",
            "device-a",
        );
        let engine_b = SyncEngine::new(
            store_b,
            s3_backend(&server),
            "vault",
            "generation",
            "device-b",
        );

        assert_eq!(
            engine_a
                .run_once(SyncTrigger::Manual)
                .await
                .unwrap()
                .published,
            1
        );
        let head: HeadDocument = serde_json::from_slice(
            &backend_a
                .get(&engine_a.head_path().unwrap())
                .await
                .unwrap()
                .bytes,
        )
        .unwrap();
        let bundle = backend_a
            .get(&RemotePath::parse(&head.path).unwrap())
            .await
            .unwrap();
        let bundle = open_bundle(&bundle.bytes, &BundleLimits::default()).unwrap();
        assert_eq!((bundle.header.start_seq, bundle.header.end_seq), (1, 2));
        assert_eq!(bundle.contents.changes[0].local_seq, 2);

        assert_eq!(
            engine_b.run_once(SyncTrigger::Manual).await.unwrap().pulled,
            1
        );
        let title: String = sqlx::query_scalar("SELECT title FROM sessions")
            .fetch_one(&pool_b)
            .await
            .unwrap();
        assert_eq!(title, "latest");
    }

    #[tokio::test]
    async fn published_head_recovers_when_local_acknowledgement_was_lost() {
        let (store, pool) = test_store("device-a").await;
        let pending = store
            .queue_local_upsert(snapshot(0, "published"), 1_000)
            .await
            .unwrap();
        let server = TestS3::start("AKID", None).await;
        initialize_test_vault(
            s3_backend(&server).as_ref(),
            "vault",
            "generation",
            VaultProtection::plain(),
        )
        .await;
        let engine = SyncEngine::new(
            store.clone(),
            s3_backend(&server),
            "vault",
            "generation",
            "device-a",
        );
        engine.run_once(SyncTrigger::Manual).await.unwrap();

        sqlx::query(
            "INSERT INTO sync_mutations
             (platform, platform_session_id, local_seq, operation, version_wall_ms,
              version_counter, version_device_id, content_hash, snapshot_json)
             VALUES (?, ?, ?, 'upsert', ?, ?, ?, ?, ?)",
        )
        .bind(&pending.key.platform)
        .bind(&pending.key.platform_session_id)
        .bind(pending.local_seq)
        .bind(pending.version.wall_ms)
        .bind(pending.version.counter)
        .bind(&pending.version.device_id)
        .bind(&pending.content_hash)
        .bind(serde_json::to_string(&pending.snapshot).unwrap())
        .execute(&pool)
        .await
        .unwrap();

        let recovered = engine.run_once(SyncTrigger::Manual).await.unwrap();
        assert_eq!((recovered.published, recovered.acknowledged), (0, 1));
        assert!(store.pending_mutations(1).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn rewrite_generation_publishes_every_entity_and_preserves_the_outbox() {
        let (store, pool) = test_store("device-a").await;
        let sessions = (0..501)
            .map(|index| NormalizedSession {
                id: format!("local-{index}"),
                platform: "chat".into(),
                platform_session_id: format!("remote-{index}"),
                title: format!("session-{index}"),
                created_at: None,
                updated_at: None,
                imported_at: "2026-07-29T00:00:00Z".into(),
                messages: vec![],
                raw_data: json!({"fixture": index}),
            })
            .collect::<Vec<_>>();
        import_sessions(&pool, &sessions, true).await.unwrap();
        store
            .queue_local_delete(
                EntityKey {
                    platform: "chat".into(),
                    platform_session_id: "deleted-session".into(),
                },
                2_000,
            )
            .await
            .unwrap();
        let pending_before = store.pending_mutations(1_000).await.unwrap();
        let server = TestS3::start("AKID", None).await;
        let backend = s3_backend(&server);
        load_or_create_vault(
            backend.as_ref(),
            VaultDocument::active(
                VaultIdentity {
                    format_version: 2,
                    vault_id: "vault".into(),
                    generation_id: "generation-old".into(),
                },
                VaultProtection::plain(),
            ),
        )
        .await
        .unwrap();
        let engine = SyncEngine::new(
            store.clone(),
            backend.clone(),
            "vault",
            "generation-old",
            "device-a",
        );

        let report = engine.rewrite_generation("generation-new").await.unwrap();

        assert_eq!(report.published, 502);
        assert_eq!(
            store.pending_mutations(1_000).await.unwrap(),
            pending_before
        );
        let head_path =
            RemotePath::parse("v1/generations/generation-new/devices/baseline/head.json").unwrap();
        let head: HeadDocument =
            serde_json::from_slice(&backend.get(&head_path).await.unwrap().bytes).unwrap();
        assert_eq!(head.generation_id, "generation-new");
        assert_eq!(head.device_id, "baseline");
        assert_eq!(head.end_seq, 502);
        let final_bundle = backend
            .get(&RemotePath::parse(&head.path).unwrap())
            .await
            .unwrap();
        let decoded = open_bundle(&final_bundle.bytes, &BundleLimits::default()).unwrap();
        assert_eq!(
            (decoded.header.start_seq, decoded.header.end_seq),
            (501, 502)
        );
        assert_eq!(decoded.header.previous_end_seq, Some(500));
        assert!(decoded.contents.changes.iter().any(|change| {
            change.key.platform_session_id == "deleted-session"
                && change.operation == crate::sync::types::MutationOperation::Delete
                && change.snapshot.is_none()
        }));
    }

    #[tokio::test]
    async fn rotate_generation_pulls_with_old_protector_and_seals_with_new_protector() {
        let (remote_store, remote_pool) = test_store("device-remote").await;
        let (local_store, local_pool) = test_store("device-local").await;
        for (pool, id, remote_id, title) in [
            (
                &remote_pool,
                "remote-local-id",
                "remote-0",
                "remote-before-rotation",
            ),
            (
                &local_pool,
                "local-local-id",
                "remote-1",
                "local-before-rotation",
            ),
        ] {
            import_sessions(
                pool,
                &[NormalizedSession {
                    id: id.into(),
                    platform: "chat".into(),
                    platform_session_id: remote_id.into(),
                    title: title.into(),
                    created_at: None,
                    updated_at: None,
                    imported_at: "2026-07-29T00:00:00Z".into(),
                    messages: vec![],
                    raw_data: json!({"fixture": title}),
                }],
                true,
            )
            .await
            .unwrap();
        }
        let server = TestS3::start("AKID", None).await;
        let backend = s3_backend(&server);
        let old_protection = test_protection("old passphrase");
        load_or_create_vault(
            backend.as_ref(),
            VaultDocument::active(
                VaultIdentity {
                    format_version: 2,
                    vault_id: "vault".into(),
                    generation_id: "generation-old".into(),
                },
                old_protection,
            ),
        )
        .await
        .unwrap();
        let old_protector = test_protector("old passphrase");
        let new_protector = test_protector("new passphrase");
        SyncEngine::new_protected(
            remote_store,
            backend.clone(),
            "vault",
            "generation-old",
            "device-remote",
            Some(old_protector.clone()),
        )
        .run_once(SyncTrigger::Manual)
        .await
        .unwrap();
        let rotating = SyncEngine::new_protected(
            local_store,
            backend.clone(),
            "vault",
            "generation-old",
            "device-local",
            Some(old_protector.clone()),
        );

        let report = rotating
            .rotate_generation(
                "generation-new",
                test_protection("new passphrase"),
                Some(new_protector.clone()),
            )
            .await
            .unwrap();

        assert_eq!(report.pulled, 1);
        assert_eq!(report.published, 2);
        let identity = load_versioned_identity(backend.as_ref()).await.unwrap();
        assert_eq!(identity.identity.generation_id, "generation-new");
        let head_path =
            RemotePath::parse("v1/generations/generation-new/devices/baseline/head.json").unwrap();
        let head: HeadDocument =
            serde_json::from_slice(&backend.get(&head_path).await.unwrap().bytes).unwrap();
        let bundle = backend
            .get(&RemotePath::parse(&head.path).unwrap())
            .await
            .unwrap();
        assert_eq!(
            open_bundle_protected(
                &bundle.bytes,
                &BundleLimits::default(),
                Some(new_protector.as_ref()),
            )
            .unwrap()
            .header
            .protection,
            ProtectionAlgorithm::XChaCha20Poly1305
        );
        assert!(
            open_bundle_protected(
                &bundle.bytes,
                &BundleLimits::default(),
                Some(old_protector.as_ref()),
            )
            .is_err()
        );
        let titles: Vec<String> = sqlx::query_scalar("SELECT title FROM sessions ORDER BY title")
            .fetch_all(&local_pool)
            .await
            .unwrap();
        assert_eq!(
            titles,
            vec!["local-before-rotation", "remote-before-rotation"]
        );
    }

    #[tokio::test]
    async fn rotate_generation_can_disable_encryption_after_old_protected_pull() {
        let (publisher_store, publisher_pool) = test_store("device-publisher").await;
        import_sessions(
            &publisher_pool,
            &[NormalizedSession {
                id: "local-id".into(),
                platform: "chat".into(),
                platform_session_id: "remote-0".into(),
                title: "encrypted-source".into(),
                created_at: None,
                updated_at: None,
                imported_at: "2026-07-29T00:00:00Z".into(),
                messages: vec![],
                raw_data: json!({"fixture": true}),
            }],
            true,
        )
        .await
        .unwrap();
        let server = TestS3::start("AKID", None).await;
        let backend = s3_backend(&server);
        let old_protection = test_protection("old passphrase");
        load_or_create_vault(
            backend.as_ref(),
            VaultDocument::active(
                VaultIdentity {
                    format_version: 2,
                    vault_id: "vault".into(),
                    generation_id: "generation-old".into(),
                },
                old_protection,
            ),
        )
        .await
        .unwrap();
        let old_protector = test_protector("old passphrase");
        SyncEngine::new_protected(
            publisher_store,
            backend.clone(),
            "vault",
            "generation-old",
            "device-publisher",
            Some(old_protector.clone()),
        )
        .run_once(SyncTrigger::Manual)
        .await
        .unwrap();
        let (rotating_store, _rotating_pool) = test_store("device-rotating").await;
        let rotating = SyncEngine::new_protected(
            rotating_store,
            backend.clone(),
            "vault",
            "generation-old",
            "device-rotating",
            Some(old_protector),
        );

        let report = rotating
            .rotate_generation("generation-plain", VaultProtection::plain(), None)
            .await
            .unwrap();

        assert_eq!((report.pulled, report.published), (1, 1));
        let head_path =
            RemotePath::parse("v1/generations/generation-plain/devices/baseline/head.json")
                .unwrap();
        let head: HeadDocument =
            serde_json::from_slice(&backend.get(&head_path).await.unwrap().bytes).unwrap();
        let bundle = backend
            .get(&RemotePath::parse(&head.path).unwrap())
            .await
            .unwrap();
        assert_eq!(
            open_bundle(&bundle.bytes, &BundleLimits::default())
                .unwrap()
                .header
                .protection,
            ProtectionAlgorithm::Plain
        );
    }

    #[tokio::test]
    async fn rotation_rejects_mismatched_target_protection_before_remote_write() {
        let (store, pool) = test_store("device-local").await;
        import_sessions(&pool, &[normalized_session(0, "preserved", "local")], true)
            .await
            .unwrap();
        let server = TestS3::start("AKID", None).await;
        let backend = s3_backend(&server);
        let active = VaultDocument::active(
            VaultIdentity {
                format_version: 2,
                vault_id: "vault".into(),
                generation_id: "generation-old".into(),
            },
            VaultProtection::plain(),
        );
        load_or_create_vault(backend.as_ref(), active.clone())
            .await
            .unwrap();
        let engine = SyncEngine::new(
            store,
            backend.clone(),
            "vault",
            "generation-old",
            "device-local",
        );

        let encrypted_without_protector = engine
            .rotate_generation(
                "generation-encrypted",
                test_protection("new passphrase"),
                None,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(encrypted_without_protector, AppError::InvalidData(ref message) if message.contains("target protection does not match target protector")),
            "{encrypted_without_protector:?}"
        );

        let plain_with_protector = engine
            .rotate_generation(
                "generation-plain",
                VaultProtection::plain(),
                Some(test_protector("new passphrase")),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(plain_with_protector, AppError::InvalidData(ref message) if message.contains("target protection does not match target protector")),
            "{plain_with_protector:?}"
        );

        assert_eq!(
            load_versioned_identity(backend.as_ref())
                .await
                .unwrap()
                .document(),
            active
        );
        for generation in ["generation-encrypted", "generation-plain"] {
            let head = RemotePath::parse(&format!(
                "v1/generations/{generation}/devices/baseline/head.json"
            ))
            .unwrap();
            assert_eq!(backend.get(&head).await.unwrap_err().kind(), "not_found");
        }
    }

    #[tokio::test]
    async fn rotate_generation_cas_failure_keeps_old_generation_readable() {
        let (store, pool) = test_store("device-local").await;
        import_sessions(
            &pool,
            &[NormalizedSession {
                id: "local-id".into(),
                platform: "chat".into(),
                platform_session_id: "remote-0".into(),
                title: "preserved".into(),
                created_at: None,
                updated_at: None,
                imported_at: "2026-07-29T00:00:00Z".into(),
                messages: vec![],
                raw_data: json!({"fixture": true}),
            }],
            true,
        )
        .await
        .unwrap();
        let server = TestS3::start("AKID", None).await;
        let backend = Arc::new(FailFirstHeadWriteBackend::failing_vault_cas(s3_backend(
            &server,
        )));
        let old_identity = VaultIdentity {
            format_version: 2,
            vault_id: "vault".into(),
            generation_id: "generation-old".into(),
        };
        load_or_create_vault(
            backend.as_ref(),
            VaultDocument::active(old_identity.clone(), test_protection("old passphrase")),
        )
        .await
        .unwrap();
        let old_protector = test_protector("old passphrase");
        let engine = SyncEngine::new_protected(
            store,
            backend.clone(),
            "vault",
            "generation-old",
            "device-local",
            Some(old_protector.clone()),
        );
        engine.run_once(SyncTrigger::Manual).await.unwrap();
        backend.arm_vault_cas_failure();

        let error = engine
            .rotate_generation(
                "generation-new",
                test_protection("new passphrase"),
                Some(test_protector("new passphrase")),
            )
            .await
            .unwrap_err();

        assert!(matches!(error, AppError::Cloud(_)));
        assert_eq!(
            load_versioned_identity(backend.as_ref())
                .await
                .unwrap()
                .identity,
            old_identity
        );
        let (reader_store, reader_pool) = test_store("device-reader").await;
        let read_report = SyncEngine::new_protected(
            reader_store,
            backend,
            "vault",
            "generation-old",
            "device-reader",
            Some(old_protector),
        )
        .run_once(SyncTrigger::Manual)
        .await
        .unwrap();
        assert_eq!(read_report.pulled, 1);
        let title: String = sqlx::query_scalar("SELECT title FROM sessions")
            .fetch_one(&reader_pool)
            .await
            .unwrap();
        assert_eq!(title, "preserved");
    }

    #[tokio::test]
    async fn rotation_outcome_is_committed_when_activation_confirmation_read_fails_once() {
        let (store, _pool) = test_store("device-local").await;
        let server = TestS3::start("AKID", None).await;
        let backend = Arc::new(FailFirstHeadWriteBackend::failing_activation_confirmation(
            s3_backend(&server),
        ));
        initialize_test_vault(
            backend.as_ref(),
            "vault",
            "generation-old",
            VaultProtection::plain(),
        )
        .await;
        let engine = SyncEngine::new(
            store,
            backend.clone(),
            "vault",
            "generation-old",
            "device-local",
        );

        let outcome = engine
            .rotate_generation_with_operation(
                "generation-confirmed",
                VaultProtection::plain(),
                None,
                "rotation-confirmed",
            )
            .await;

        let committed = matches!(
            &outcome,
            RotationOutcome::Committed {
                operation_id,
                vault,
                ..
            } if operation_id == "rotation-confirmed"
                && vault.identity.generation_id == "generation-confirmed"
                && vault.state == VaultState::Active
        );
        assert!(committed, "unexpected rotation outcome: {outcome:?}");
        assert_eq!(
            load_versioned_identity(backend.as_ref())
                .await
                .unwrap()
                .identity
                .generation_id,
            "generation-confirmed"
        );
    }

    #[tokio::test]
    async fn invalid_device_hash_is_isolated_while_other_devices_converge() {
        let (bad_store, _bad_pool) = test_store("device-bad").await;
        let (good_store, _good_pool) = test_store("device-good").await;
        let (local_store, local_pool) = test_store("device-local").await;
        bad_store
            .queue_local_upsert(snapshot(0, "bad-source"), 1_000)
            .await
            .unwrap();
        good_store
            .queue_local_upsert(snapshot(1, "good-source"), 1_000)
            .await
            .unwrap();
        let server = TestS3::start("AKID", None).await;
        let shared_backend = s3_backend(&server);
        initialize_test_vault(
            shared_backend.as_ref(),
            "vault",
            "generation",
            VaultProtection::plain(),
        )
        .await;
        let bad_engine = SyncEngine::new(
            bad_store,
            shared_backend.clone(),
            "vault",
            "generation",
            "device-bad",
        );
        let good_engine = SyncEngine::new(
            good_store,
            shared_backend.clone(),
            "vault",
            "generation",
            "device-good",
        );
        bad_engine.run_once(SyncTrigger::Manual).await.unwrap();
        good_engine.run_once(SyncTrigger::Manual).await.unwrap();

        let bad_head_path = bad_engine.head_path().unwrap();
        let existing = shared_backend.get(&bad_head_path).await.unwrap();
        let mut bad_head: HeadDocument = serde_json::from_slice(&existing.bytes).unwrap();
        bad_head.sha256 = "00".repeat(32);
        shared_backend
            .put_if_match(
                &bad_head_path,
                &serde_json::to_vec(&bad_head).unwrap(),
                existing.etag.as_deref().unwrap(),
            )
            .await
            .unwrap();

        let local_engine = SyncEngine::new(
            local_store,
            s3_backend(&server),
            "vault",
            "generation",
            "device-local",
        );
        let report = local_engine.run_once(SyncTrigger::Manual).await.unwrap();
        assert_eq!(report.pulled, 1);
        let titles: Vec<String> = sqlx::query_scalar("SELECT title FROM sessions ORDER BY title")
            .fetch_all(&local_pool)
            .await
            .unwrap();
        assert_eq!(titles, vec!["good-source"]);
    }

    #[tokio::test]
    async fn generation_rotation_aborts_when_any_remote_device_chain_is_corrupt() {
        let server = TestS3::start("AKID", None).await;
        let shared_backend = s3_backend(&server);
        let active = VaultDocument::active(
            VaultIdentity {
                format_version: 2,
                vault_id: "vault".into(),
                generation_id: "generation".into(),
            },
            VaultProtection::plain(),
        );
        load_or_create_vault(shared_backend.as_ref(), active.clone())
            .await
            .unwrap();
        let (bad_store, _bad_pool) = test_store("device-bad").await;
        bad_store
            .queue_local_upsert(snapshot(0, "only-on-corrupt-device"), 1_000)
            .await
            .unwrap();
        let bad_engine = SyncEngine::new(
            bad_store,
            shared_backend.clone(),
            "vault",
            "generation",
            "device-bad",
        );
        bad_engine.run_once(SyncTrigger::Manual).await.unwrap();
        let bad_head_path = bad_engine.head_path().unwrap();
        let existing = shared_backend.get(&bad_head_path).await.unwrap();
        let mut bad_head: HeadDocument = serde_json::from_slice(&existing.bytes).unwrap();
        bad_head.sha256 = "00".repeat(32);
        shared_backend
            .put_if_match(
                &bad_head_path,
                &serde_json::to_vec(&bad_head).unwrap(),
                existing.etag.as_deref().unwrap(),
            )
            .await
            .unwrap();
        let (rotating_store, _rotating_pool) = test_store("device-rotating").await;
        let rotating = SyncEngine::new(
            rotating_store,
            shared_backend.clone(),
            "vault",
            "generation",
            "device-rotating",
        );

        let outcome = rotating
            .rotate_generation_with_operation(
                "generation-next",
                VaultProtection::plain(),
                None,
                "rotation-strict-corrupt",
            )
            .await;
        assert!(matches!(
            outcome,
            RotationOutcome::RolledBack {
                ref operation_id,
                ref vault,
                error: AppError::InvalidData(_),
            } if operation_id == "rotation-strict-corrupt"
                && vault.identity.generation_id == "generation"
                && vault.state == VaultState::Active
        ));
        assert_eq!(
            load_versioned_identity(shared_backend.as_ref())
                .await
                .unwrap()
                .document(),
            active
        );
        let next_head =
            RemotePath::parse("v1/generations/generation-next/devices/baseline/head.json").unwrap();
        assert!(shared_backend.get(&next_head).await.is_err());
    }
}
