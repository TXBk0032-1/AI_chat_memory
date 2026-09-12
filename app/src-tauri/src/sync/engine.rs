use crate::{
    error::{AppError, Result},
    semantic::engine::SemanticEngine,
    sync::{
        backend::{CloudBackend, RemotePath},
        bundle::{
            BundleHeader, BundleLimits, DecodedBundle, PreviousValidation, ProtectionAlgorithm,
            SealedBundle, is_bundle_limit_error, open_bundle_protected,
            open_released_v1_unchained_bundle_protected, parse_bundle_header,
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
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::Mutex;

const MAX_MUTATIONS_PER_BUNDLE: usize = 500;

/// Cumulative cap on the raw (still-encrypted) envelope bytes downloaded while
/// pulling a single device's bundle chain. Protects against a malicious or
/// runaway remote pinning arbitrarily many near-max-size bundles into memory.
/// 1 GiB bounds a long legitimate chain while rejecting an adversarial flood.
const MAX_PULL_CHAIN_BYTES: usize = 1024 * 1024 * 1024;

/// Cap on candidate bundles inspected while reconstructing a released-v1 device
/// history. The legacy bundles directory is enumerated without an inherent bound,
/// so an adversary who drops many `.acmb` objects there could force unbounded
/// download/decrypt.
const MAX_RELEASED_V1_CANDIDATES: usize = 1000;
const MAX_RELEASED_V1_BYTES: usize = 1024 * 1024 * 1024;
const MAX_RELEASED_V1_EVENTS: usize = 500_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeadDocument {
    pub generation_id: String,
    pub device_id: String,
    pub end_seq: i64,
    pub path: String,
    pub sha256: String,
}

/// A downloaded remote bundle kept as its raw envelope plus the validated
/// plaintext header only. The decrypted, decompressed, deserialized heap is
/// produced on demand at apply time so a pull chain never holds more than one
/// decoded bundle in memory.
struct RemoteBundleDownload {
    /// Compact raw envelope bytes; the cumulative size is capped by
    /// MAX_PULL_CHAIN_BYTES during the chain walk.
    raw: Vec<u8>,
    /// Plaintext header validated by `parse_bundle_header` at download time.
    /// Authenticity of these fields is only enforced at apply time via the
    /// AAD-bound open.
    header: BundleHeader,
    released_v1_unchained: bool,
    path: String,
    sha256: String,
}

/// Result of rebuilding a released-v1 writer's history: the legacy head is
/// decoded once with its filtered pending changes, and the strict suffix keeps
/// raw envelopes for streaming apply.
struct ReleasedV1Replay {
    head: DecodedBundle,
    head_path: String,
    head_sha256: String,
    suffix: Vec<RemoteBundleDownload>,
}

struct ReleasedV1BundleCandidate {
    path: String,
    sha256: String,
    bundle: RemoteBundleDownload,
}

/// A bundle that has been sealed (and, in the fresh-seal path, staged) and is
/// ready for upload + head publication. Shared between the staged-reuse and
/// fresh-seal publish paths so the crypto-failure reseal can reuse the
/// same finalization logic.
struct PreparedBundle {
    path: RemotePath,
    digest: String,
    bytes: Vec<u8>,
    end_seq: i64,
    published_mutations: Vec<PendingMutation>,
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
    semantic: Option<Arc<SemanticEngine>>,
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
            semantic: None,
        }
    }

    /// Attaches the semantic engine so pulled remote snapshots wake up vector
    /// re-indexing and stale vectors get cleaned through the merge path.
    pub fn with_semantic(mut self, semantic: Option<Arc<SemanticEngine>>) -> Self {
        self.semantic = semantic;
        self
    }

    #[cfg(test)]
    fn with_bundle_limits(mut self, bundle_limits: BundleLimits) -> Self {
        self.bundle_limits = bundle_limits;
        self
    }

    pub async fn run_once(&self, _trigger: SyncTrigger) -> Result<SyncReport> {
        let _guard = self.single_flight.lock().await;
        let (vault, recovered) = self.ensure_active_vault().await?;
        log_recovered_publication(self.device_id.as_str(), recovered.as_ref());
        let mut report = self
            .pull_remote(PullPolicy::TolerantSync, vault.compatibility)
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

    pub async fn run_once_with_generation_replay(
        &self,
        _trigger: SyncTrigger,
    ) -> Result<SyncReport> {
        let _guard = self.single_flight.lock().await;
        let (vault, recovered) = self.ensure_active_vault().await?;
        log_recovered_publication(self.device_id.as_str(), recovered.as_ref());
        let mut report = self
            .pull_remote(PullPolicy::TolerantSync, vault.compatibility)
            .await?;
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
        let merger = MergeEngine::new(self.store.pool().clone(), self.semantic.clone());
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
        let mut chain_bytes: usize = 0;
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
            chain_bytes = chain_bytes.saturating_add(downloaded.raw.len());
            if chain_bytes > MAX_PULL_CHAIN_BYTES {
                return Err(AppError::InvalidData(
                    "remote bundle chain exceeded the cumulative byte budget".into(),
                ));
            }
            if downloaded.released_v1_unchained {
                released_v1_boundary = Some((document, downloaded));
                break;
            }
            // The plaintext header is all the walk needs; the decrypted heap
            // stays untouched until apply time.
            current = match (
                downloaded.header.previous_path.clone(),
                downloaded.header.previous_sha256.clone(),
                downloaded.header.previous_end_seq,
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
        let mut pulled = 0;
        // Streaming apply: at most one bundle's decrypted heap is alive at any
        // moment; the raw envelopes resident in memory are bounded by
        // MAX_PULL_CHAIN_BYTES.
        match released_v1_boundary {
            Some((boundary, downloaded)) => {
                let replay = self
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
                let mut head = replay.head;
                let expected = self
                    .store
                    .remote_cursor(&self.generation_id, remote_device_id)
                    .await?
                    .map(|value| value.cursor_seq)
                    .unwrap_or(cursor);
                let covered_start = expected
                    .checked_add(1)
                    .ok_or_else(|| AppError::InvalidData("released v1 cursor overflow".into()))?;
                head.header.start_seq = covered_start;
                head.contents.start_seq = covered_start;
                head.header.previous_path = None;
                head.header.previous_sha256 = None;
                head.header.previous_end_seq = (expected > 0).then_some(expected);
                head.contents.previous_path = None;
                head.contents.previous_sha256 = None;
                head.contents.previous_end_seq = head.header.previous_end_seq;
                let anchor = RemoteObjectAnchor {
                    end_seq: head.header.end_seq,
                    path: replay.head_path,
                    sha256: replay.head_sha256,
                };
                let outcome = merger
                    .apply_bundle(
                        &self.generation_id,
                        remote_device_id,
                        expected,
                        &head,
                        &anchor,
                    )
                    .await?;
                pulled += outcome.applied;
                for downloaded in replay.suffix {
                    pulled += self
                        .decode_and_apply_bundle(merger, remote_device_id, downloaded)
                        .await?;
                }
            }
            None => {
                for downloaded in chain.into_iter().rev() {
                    pulled += self
                        .decode_and_apply_bundle(merger, remote_device_id, downloaded)
                        .await?;
                }
            }
        }
        Ok(pulled)
    }

    /// Decodes one raw envelope — enforcing, through the AAD-bound open, the
    /// authenticity of the header fields that drove the chain walk — and
    /// applies it against the live cursor. The decoded heap is dropped as soon
    /// as the call returns.
    async fn decode_and_apply_bundle(
        &self,
        merger: &MergeEngine,
        remote_device_id: &str,
        downloaded: RemoteBundleDownload,
    ) -> Result<usize> {
        let expected = self
            .store
            .remote_cursor(&self.generation_id, remote_device_id)
            .await?
            .map(|value| value.cursor_seq)
            .unwrap_or(0);
        let decoded = if downloaded.released_v1_unchained {
            open_released_v1_unchained_bundle_protected(
                &downloaded.raw,
                &self.bundle_limits,
                self.protector.as_deref(),
            )?
        } else {
            open_bundle_protected(
                &downloaded.raw,
                &self.bundle_limits,
                self.protector.as_deref(),
            )?
        };
        let anchor = RemoteObjectAnchor {
            end_seq: decoded.header.end_seq,
            path: downloaded.path,
            sha256: downloaded.sha256,
        };
        let outcome = merger
            .apply_bundle(
                &self.generation_id,
                remote_device_id,
                expected,
                &decoded,
                &anchor,
            )
            .await?;
        Ok(outcome.applied)
    }

    async fn reconstruct_released_v1_history(
        &self,
        reconstruction: ReleasedV1Reconstruction<'_>,
        legacy_head_bundle: RemoteBundleDownload,
        strict_suffix: Vec<RemoteBundleDownload>,
    ) -> Result<ReleasedV1Replay> {
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
        let mut candidate_bytes: usize = 0;
        // Content-addressed object names carry the bundle SHA-256, so a
        // second listing entry with an already-verified digest is a duplicate
        // copy of the same content. Skip it before downloading instead of
        // paying a second download+decode for identical bytes.
        let mut verified_digests = std::collections::HashSet::new();
        for entry in entries {
            if entry.is_collection || !entry.name.ends_with(".acmb") {
                continue;
            }
            if candidates.len() >= MAX_RELEASED_V1_CANDIDATES {
                return Err(AppError::InvalidData(
                    "released v1 bundle listing exceeded the candidate limit".into(),
                ));
            }
            let (start_seq, end_seq, sha256) = parse_bundle_object_name(&entry.name)?;
            if start_seq > legacy_head.end_seq || end_seq > legacy_head.end_seq {
                continue;
            }
            if !verified_digests.insert(sha256.clone()) {
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
            candidate_bytes = candidate_bytes.saturating_add(bundle.raw.len());
            if candidate_bytes > MAX_RELEASED_V1_BYTES {
                return Err(AppError::InvalidData(
                    "released v1 bundle download exceeded the cumulative byte budget".into(),
                ));
            }
            if bundle.header.start_seq != start_seq {
                return Err(AppError::InvalidData(
                    "released v1 bundle filename does not match its sequence range".into(),
                ));
            }
            let has_no_previous = bundle.header.previous_path.is_none()
                && bundle.header.previous_sha256.is_none()
                && bundle.header.previous_end_seq.is_none();
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
                && candidate.bundle.header.end_seq == legacy_head.end_seq
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
                    && candidate.bundle.header.end_seq == anchor.end_seq
            }) {
                return Err(AppError::SyncProtocol(
                    "released v1 immutable history no longer contains the persisted cursor anchor"
                        .into(),
                ));
            }
        }

        // Decode one candidate at a time; only the deduplicated event map
        // (bounded by MAX_RELEASED_V1_EVENTS) stays resident across candidates.
        let mut unique_events = BTreeMap::<i64, BundleChange>::new();
        for candidate in &candidates {
            let decoded = self.open_stored_bundle(&candidate.bundle)?;
            for change in &decoded.contents.changes {
                match unique_events.get(&change.local_seq) {
                    Some(existing) if existing == change => {}
                    Some(_) => {
                        return Err(AppError::SyncProtocol(
                            "released v1 bundle history is ambiguous: conflicting same sequence events"
                                .into(),
                        ));
                    }
                    None => {
                        if unique_events.len() >= MAX_RELEASED_V1_EVENTS {
                            return Err(AppError::InvalidData(
                                "released v1 history exceeded the event limit".into(),
                            ));
                        }
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

        // The legacy head is decoded exactly once here; its surviving decoded
        // heap is the only one carried out of the reconstruction.
        let mut head = self.open_stored_bundle(&legacy_head_bundle)?;
        head.contents.changes = pending;
        head.contents.start_seq = head
            .contents
            .changes
            .first()
            .map(|change| change.local_seq)
            .ok_or_else(|| AppError::InvalidData("released v1 history is empty".into()))?;
        head.header.start_seq = head.contents.start_seq;

        if let Some(oldest_current) = strict_suffix.last() {
            let header = &oldest_current.header;
            if header.previous_path.as_deref() != Some(legacy_head.path.as_str())
                || header.previous_sha256.as_deref() != Some(legacy_head.sha256.as_str())
                || header.previous_end_seq != Some(legacy_head.end_seq)
            {
                return Err(AppError::SyncProtocol(
                    "current bundle chain conflicts with reconstructed released v1 history".into(),
                ));
            }
        }
        Ok(ReleasedV1Replay {
            head,
            head_path: legacy_head.path.clone(),
            head_sha256: legacy_head.sha256.clone(),
            suffix: strict_suffix.into_iter().rev().collect(),
        })
    }

    /// Opens a downloaded bundle with the variant its header classification
    /// selected at download time, so the decode is guaranteed to succeed
    /// whenever the header parse did.
    fn open_stored_bundle(&self, bundle: &RemoteBundleDownload) -> Result<DecodedBundle> {
        if bundle.released_v1_unchained {
            open_released_v1_unchained_bundle_protected(
                &bundle.raw,
                &self.bundle_limits,
                self.protector.as_deref(),
            )
        } else {
            open_bundle_protected(&bundle.raw, &self.bundle_limits, self.protector.as_deref())
        }
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
        // Single-bundle decode for the caller; the heap is dropped on return.
        open_bundle_protected(
            &downloaded.raw,
            &self.bundle_limits,
            self.protector.as_deref(),
        )
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
        // Structural header validation only: the plaintext header drives the
        // chain walk, while authenticity is enforced later at apply time by
        // the AAD-bound decrypt.
        let (header, released_v1_unchained) = match parse_bundle_header(
            &object.bytes,
            &self.bundle_limits,
            self.protector.as_deref(),
            PreviousValidation::StrictChain,
        ) {
            Ok(header) => (header, false),
            Err(strict_error) => {
                match parse_bundle_header(
                    &object.bytes,
                    &self.bundle_limits,
                    self.protector.as_deref(),
                    PreviousValidation::ReleasedV1Unchained,
                ) {
                    Ok(header) if compatibility == Some(VaultCompatibility::ReleasedV1Writers) => {
                        (header, true)
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
        if header.vault_id != self.vault_id
            || header.generation_id != self.generation_id
            || header.device_id != remote_device_id
            || header.end_seq != document.end_seq
        {
            return Err(AppError::InvalidData(
                "remote bundle does not match its chain reference".into(),
            ));
        }
        Ok(RemoteBundleDownload {
            raw: object.bytes,
            header,
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
        // 恢复残留发布后必须继续走正常发布流程——recovered_publication 只作
        // 幂等背景（已推进的 head 会经 already_published 分支转为 acknowledge），
        // 提前返回会让恢复期间积压的新 pending 被跳过到下一次 run。
        let (publication_vault, _recovered_publication) = self.ensure_active_vault().await?;
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
            // already_published 由 previous_head.map(..).unwrap_or(0) 推出，大于 0 必有 head。
            let head = &previous_head
                .as_ref()
                .expect("already_published > 0 implies previous_head is Some")
                .0;
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
                self.discard_staged_bundle(&staged.bundle_sha256).await;
                return Err(AppError::InvalidData(
                    "staged bundle SHA-256 does not match its bytes".into(),
                ));
            }
            // A Crypto error here means the staged bundle was sealed with a protector
            // we no longer hold (e.g. the passphrase rotated). The staged bytes are
            // permanently unrecoverable, so discard the record and reseal a fresh
            // bundle from the current outbox instead of pinning the publish loop
            // against a bad staged row forever.
            let decoded = match open_bundle_protected(
                &staged.bundle_bytes,
                &self.bundle_limits,
                self.protector.as_deref(),
            ) {
                Ok(decoded) => decoded,
                Err(AppError::Crypto(_)) => {
                    self.discard_staged_bundle(&staged.bundle_sha256).await;
                    tracing::warn!(
                        bundle_sha256 = %staged.bundle_sha256,
                        "staged bundle could not be decrypted with current protector; resealing"
                    );
                    let prepared = self
                        .seal_and_stage_fresh_bundle(
                            pending,
                            previous_head.as_ref().map(|(head, _etag)| head),
                        )
                        .await?;
                    return self
                        .finalize_publish(
                            prepared,
                            previous_head.as_ref(),
                            &head_path,
                            publication_vault,
                            acknowledged,
                        )
                        .await;
                }
                Err(error) => return Err(error),
            };
            if decoded.header.vault_id != self.vault_id
                || decoded.header.generation_id != staged.generation_id
                || decoded.header.device_id != staged.device_id
                || decoded.header.start_seq != staged.start_seq
                || decoded.header.end_seq != staged.end_seq
                || staged.generation_id != self.generation_id
                || staged.device_id != self.device_id
            {
                self.discard_staged_bundle(&staged.bundle_sha256).await;
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
                self.discard_staged_bundle(&staged.bundle_sha256).await;
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
                self.discard_staged_bundle(&staged.bundle_sha256).await;
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
                // The outbox prefix changed (e.g. via coalescing) since this bundle
                // was staged, so it no longer matches what we need to publish.
                // Discard the orphaned staged row so the next attempt reseals fresh
                // instead of retrying against a prefix that will never match.
                self.discard_staged_bundle(&staged.bundle_sha256).await;
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
            let prepared = self
                .seal_and_stage_fresh_bundle(
                    pending,
                    previous_head.as_ref().map(|(head, _etag)| head),
                )
                .await?;
            (
                prepared.path,
                prepared.digest,
                prepared.bytes,
                prepared.end_seq,
                prepared.published_mutations,
            )
        };
        let prepared = PreparedBundle {
            path,
            digest,
            bytes,
            end_seq,
            published_mutations,
        };
        self.finalize_publish(
            prepared,
            previous_head.as_ref(),
            &head_path,
            publication_vault,
            acknowledged,
        )
        .await
    }

    /// Seals the largest publishable prefix of `pending` into a fresh bundle and
    /// records it as `stage='staged'` so a crash between staging and head publish
    /// can be recovered on the next `publish_pending`. Used both by the normal
    /// publish path and the crypto-failure reseal path.
    async fn seal_and_stage_fresh_bundle(
        &self,
        pending: &[PendingMutation],
        previous_head: Option<&HeadDocument>,
    ) -> Result<PreparedBundle> {
        let (contents, sealed, published_count) = self.seal_largest_mutation_prefix(
            pending,
            previous_head,
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
        Ok(PreparedBundle {
            path,
            digest,
            bytes,
            end_seq,
            published_mutations,
        })
    }

    /// Uploads the prepared bundle, advances the remote head, marks it published,
    /// and acknowledges the mutations. Shared by the staged-reuse and fresh-seal
    /// publish paths.
    async fn finalize_publish(
        &self,
        prepared: PreparedBundle,
        previous_head: Option<&(HeadDocument, String)>,
        head_path: &RemotePath,
        publication_vault: VersionedVaultIdentity,
        mut acknowledged: usize,
    ) -> Result<SyncReport> {
        let PreparedBundle {
            path,
            digest,
            bytes,
            end_seq,
            published_mutations,
        } = prepared;
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
                expected_head_etag: previous_head.map(|(_head, etag)| etag.clone()),
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

    /// Best-effort removal of a stale `stage='staged'` row so the next
    /// `publish_pending` reseals from the current outbox instead of retrying
    /// against a staged bundle that can never be reused.
    /// Errors are logged, not propagated: cleanup must not mask the original
    /// publish failure that triggered it.
    async fn discard_staged_bundle(&self, bundle_sha256: &str) {
        if let Err(error) = self.store.remove_staged_bundle(bundle_sha256).await {
            tracing::warn!(%error, bundle_sha256, "failed to discard stale staged bundle");
        }
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
                head_path,
                ..
            } => {
                // The recorded published_mutation_count is what the
                // interrupted publisher requested, not what the recovered head
                // actually advances. Snapshot this device head before the
                // recovery and diff it against the head after the recovery so
                // the reported count is the real end_seq advance.
                let previous_end_seq = self.read_head_end_seq(head_path).await?;
                let recovered = recover_head_publish(self.backend.as_ref(), &current).await?;
                let recovered_end_seq = self.read_head_end_seq(head_path).await?;
                let count = recovered_end_seq
                    .unwrap_or(0)
                    .saturating_sub(previous_end_seq.unwrap_or(0));
                recovered_publication = Some((owner_device_id.clone(), count as usize));
                recovered
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

    /// Reads the `end_seq` of a device head document, returning `None` when the
    /// head does not exist yet. Used to measure how far a head-publication
    /// recovery actually advances the remote chain.
    async fn read_head_end_seq(&self, head_path: &str) -> Result<Option<i64>> {
        let path = RemotePath::parse(head_path)
            .map_err(|error| AppError::InvalidData(error.to_string()))?;
        match self.backend.get(&path).await {
            Ok(object) => {
                let head: HeadDocument = serde_json::from_slice(&object.bytes)?;
                Ok(Some(head.end_seq))
            }
            Err(error) if error.kind() == "not_found" => Ok(None),
            Err(error) => Err(cloud_error(error)),
        }
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
            let nonce = rand::rng().random::<[u8; 24]>();
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

/// Recovering a stuck head publication publishes nothing new — the
/// replayed head was already accounted for by the attempt that wrote it, and
/// `publish_pending` re-acknowledges the recovered prefix idempotently. The
/// recovered count is therefore only logged, never added to `SyncReport`.
fn log_recovered_publication(local_device_id: &str, recovered: Option<&(String, usize)>) {
    if let Some((owner_device_id, count)) = recovered {
        tracing::debug!(
            owner_device_id = %owner_device_id,
            local_device_id = %local_device_id,
            recovered_end_seq_advance = count,
            "recovered a stuck head publication"
        );
    }
}

#[cfg(test)]
mod tests;
