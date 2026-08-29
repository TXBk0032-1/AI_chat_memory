use crate::{
    error::{AppError, Result},
    sync::{
        backend::{CloudBackend, CloudError, RemotePath},
        bundle::ProtectionAlgorithm,
        crypto::{Argon2idConfig, PayloadProtector, XChaChaProtector},
    },
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

const VAULT_FORMAT_VERSION: u8 = 2;
const VERIFIER_NONCE: [u8; 24] = [0x56; 24];
const VERIFIER_PLAINTEXT: &[u8] = b"vault-verifier-v2";
const DEFAULT_MEMORY_KIB: u32 = 64 * 1024;
const DEFAULT_ITERATIONS: u32 = 3;
const DEFAULT_PARALLELISM: u32 = 1;
pub const DEFAULT_MAINTENANCE_LEASE_MS: i64 = 30 * 60 * 1000;
const MAX_KDF_MEMORY_KIB: u32 = 128 * 1024;
const MAX_KDF_ITERATIONS: u32 = 6;
const MAX_KDF_PARALLELISM: u32 = 4;
const MAX_HEAD_DOCUMENT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultIdentity {
    pub format_version: u8,
    pub vault_id: String,
    pub generation_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VaultKdfAlgorithm {
    Argon2id,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultKdf {
    pub algorithm: VaultKdfAlgorithm,
    pub version: u8,
    pub salt_hex: String,
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultProtection {
    pub algorithm: ProtectionAlgorithm,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kdf: Option<VaultKdf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verifier_hex: Option<String>,
}

impl Default for VaultProtection {
    fn default() -> Self {
        Self::plain()
    }
}

impl VaultProtection {
    pub fn plain() -> Self {
        Self {
            algorithm: ProtectionAlgorithm::Plain,
            kdf: None,
            verifier_hex: None,
        }
    }

    pub fn encrypted(vault_id: &str, passphrase: &str) -> Result<Self> {
        if !valid_id(vault_id) {
            return Err(AppError::InvalidData("vault identity is invalid".into()));
        }
        let mut salt = [0_u8; 16];
        rand::rng().fill_bytes(&mut salt);
        Self::encrypted_with_config(
            vault_id,
            passphrase,
            Argon2idConfig {
                salt,
                memory_kib: DEFAULT_MEMORY_KIB,
                iterations: DEFAULT_ITERATIONS,
                parallelism: DEFAULT_PARALLELISM,
            },
        )
    }

    pub(crate) fn encrypted_with_config(
        vault_id: &str,
        passphrase: &str,
        config: Argon2idConfig,
    ) -> Result<Self> {
        if !valid_id(vault_id) {
            return Err(AppError::InvalidData("vault identity is invalid".into()));
        }
        let mut protection = Self {
            algorithm: ProtectionAlgorithm::XChaCha20Poly1305,
            kdf: Some(VaultKdf {
                algorithm: VaultKdfAlgorithm::Argon2id,
                version: 1,
                salt_hex: hex::encode(config.salt),
                memory_kib: config.memory_kib,
                iterations: config.iterations,
                parallelism: config.parallelism,
            }),
            verifier_hex: None,
        };
        let protector = protection.derive_unverified(passphrase)?;
        let verifier = protector.seal(
            &verifier_associated_data(vault_id),
            VERIFIER_PLAINTEXT,
            VERIFIER_NONCE,
        )?;
        protection.verifier_hex = Some(hex::encode(verifier));
        protection.validate()?;
        Ok(protection)
    }

    pub fn derive_protector(
        &self,
        vault_id: &str,
        passphrase: &str,
    ) -> Result<Option<Arc<dyn PayloadProtector>>> {
        self.validate()?;
        if self.algorithm == ProtectionAlgorithm::Plain {
            return Ok(None);
        }
        let protector = self.derive_unverified(passphrase)?;
        let verifier =
            hex::decode(self.verifier_hex.as_deref().ok_or_else(|| {
                AppError::InvalidData("encrypted vault verifier is missing".into())
            })?)
            .map_err(|_| AppError::InvalidData("encrypted vault verifier is invalid".into()))?;
        let opened = protector
            .open(
                &verifier_associated_data(vault_id),
                &verifier,
                VERIFIER_NONCE,
            )
            .map_err(|_| AppError::Crypto("sync passphrase does not match remote vault".into()))?;
        if opened != VERIFIER_PLAINTEXT {
            return Err(AppError::Crypto(
                "sync passphrase does not match remote vault".into(),
            ));
        }
        Ok(Some(protector))
    }

    pub fn passphrase_matches(&self, vault_id: &str, passphrase: &str) -> Result<bool> {
        match self.derive_protector(vault_id, passphrase) {
            Ok(_) => Ok(true),
            Err(AppError::Crypto(ref msg))
                if msg == "sync passphrase does not match remote vault" =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    fn derive_unverified(&self, passphrase: &str) -> Result<Arc<dyn PayloadProtector>> {
        let kdf = self
            .kdf
            .as_ref()
            .ok_or_else(|| AppError::InvalidData("encrypted vault KDF is missing".into()))?;
        let salt_bytes = hex::decode(&kdf.salt_hex)
            .map_err(|_| AppError::InvalidData("encrypted vault KDF salt is invalid".into()))?;
        let salt: [u8; 16] = salt_bytes
            .try_into()
            .map_err(|_| AppError::InvalidData("encrypted vault KDF salt is invalid".into()))?;
        Ok(Arc::new(XChaChaProtector::derive(
            passphrase,
            &Argon2idConfig {
                salt,
                memory_kib: kdf.memory_kib,
                iterations: kdf.iterations,
                parallelism: kdf.parallelism,
            },
        )?))
    }

    fn validate(&self) -> Result<()> {
        match self.algorithm {
            ProtectionAlgorithm::Plain => {
                if self.kdf.is_some() || self.verifier_hex.is_some() {
                    return Err(AppError::InvalidData(
                        "plain vault must not contain encryption metadata".into(),
                    ));
                }
            }
            ProtectionAlgorithm::XChaCha20Poly1305 => {
                let kdf = self.kdf.as_ref().ok_or_else(|| {
                    AppError::InvalidData("encrypted vault KDF is missing".into())
                })?;
                if kdf.algorithm != VaultKdfAlgorithm::Argon2id
                    || kdf.version != 1
                    || !(8 * 1024..=MAX_KDF_MEMORY_KIB).contains(&kdf.memory_kib)
                    || !(1..=MAX_KDF_ITERATIONS).contains(&kdf.iterations)
                    || !(1..=MAX_KDF_PARALLELISM).contains(&kdf.parallelism)
                {
                    return Err(AppError::InvalidData(
                        "encrypted vault KDF parameters are unsupported".into(),
                    ));
                }
                let salt = hex::decode(&kdf.salt_hex).map_err(|_| {
                    AppError::InvalidData("encrypted vault KDF salt is invalid".into())
                })?;
                if salt.len() != 16 {
                    return Err(AppError::InvalidData(
                        "encrypted vault KDF salt is invalid".into(),
                    ));
                }
                let verifier = self.verifier_hex.as_deref().ok_or_else(|| {
                    AppError::InvalidData("encrypted vault verifier is missing".into())
                })?;
                let verifier = hex::decode(verifier).map_err(|_| {
                    AppError::InvalidData("encrypted vault verifier is invalid".into())
                })?;
                if verifier.len() != VERIFIER_PLAINTEXT.len() + 16 {
                    return Err(AppError::InvalidData(
                        "encrypted vault verifier is invalid".into(),
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum GenerationMaintenanceStage {
    #[default]
    BuildingBaseline,
    ReadyToActivate,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VaultCompatibility {
    ReleasedV1Writers,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum VaultState {
    #[default]
    Active,
    Publishing {
        operation_id: String,
        owner_device_id: String,
        started_at_ms: i64,
        lease_expires_at_ms: i64,
        head_path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        expected_head_etag: Option<String>,
        replacement_head_json: String,
        published_mutation_count: usize,
    },
    Frozen {
        operation_id: String,
        owner_device_id: String,
        started_at_ms: i64,
        lease_expires_at_ms: i64,
        target_generation_id: String,
        target_protection: VaultProtection,
        stage: GenerationMaintenanceStage,
        #[serde(default, skip_serializing_if = "is_false")]
        retire_released_v1_compatibility: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadPublishRequest {
    pub operation_id: String,
    pub owner_device_id: String,
    pub started_at_ms: i64,
    pub lease_expires_at_ms: i64,
    pub head_path: String,
    pub expected_head_etag: Option<String>,
    pub replacement_head_json: String,
    pub published_mutation_count: usize,
}

#[derive(Debug)]
pub enum VaultUpdateOutcome {
    Committed(VersionedVaultIdentity),
    Rejected {
        current: VersionedVaultIdentity,
        error: AppError,
    },
    Unknown(AppError),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultDocument {
    #[serde(flatten)]
    pub identity: VaultIdentity,
    pub protection: VaultProtection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<VaultCompatibility>,
    #[serde(default)]
    pub state: VaultState,
}

impl VaultDocument {
    pub fn active(identity: VaultIdentity, protection: VaultProtection) -> Self {
        Self {
            identity,
            protection,
            compatibility: None,
            state: VaultState::Active,
        }
    }

    pub fn released_v1_compatible(identity: VaultIdentity) -> Self {
        Self {
            identity,
            protection: VaultProtection::plain(),
            compatibility: Some(VaultCompatibility::ReleasedV1Writers),
            state: VaultState::Active,
        }
    }

    pub fn released_v1_compatibility_active(&self) -> bool {
        self.compatibility == Some(VaultCompatibility::ReleasedV1Writers)
    }

    fn validate(&self) -> Result<()> {
        self.identity.validate()?;
        self.protection.validate()?;
        if self.released_v1_compatibility_active()
            && (self.identity.vault_id != "default"
                || self.identity.generation_id != "generation-1"
                || self.protection.algorithm != ProtectionAlgorithm::Plain)
        {
            return Err(AppError::InvalidData(
                "released v1 compatibility requires the plain default generation".into(),
            ));
        }
        match &self.state {
            VaultState::Active => {}
            VaultState::Publishing {
                operation_id,
                owner_device_id,
                started_at_ms,
                lease_expires_at_ms,
                head_path,
                expected_head_etag,
                replacement_head_json,
                published_mutation_count,
            } => {
                if !valid_id(operation_id)
                    || !valid_id(owner_device_id)
                    || *started_at_ms < 0
                    || lease_expires_at_ms <= started_at_ms
                    || expected_head_etag.as_ref().is_some_and(String::is_empty)
                    || replacement_head_json.is_empty()
                    || replacement_head_json.len() > MAX_HEAD_DOCUMENT_BYTES
                    || !(1..=500).contains(published_mutation_count)
                {
                    return Err(AppError::InvalidData(
                        "remote vault publishing state is invalid".into(),
                    ));
                }
                let path = RemotePath::parse(head_path)
                    .map_err(|_| AppError::InvalidData("remote head path is invalid".into()))?;
                let expected_path = format!(
                    "v1/generations/{}/devices/{owner_device_id}/head.json",
                    self.identity.generation_id
                );
                if path.display() != expected_path
                    || serde_json::from_str::<serde_json::Value>(replacement_head_json).is_err()
                {
                    return Err(AppError::InvalidData(
                        "remote vault publishing state is invalid".into(),
                    ));
                }
            }
            VaultState::Frozen {
                operation_id,
                owner_device_id,
                started_at_ms,
                lease_expires_at_ms,
                target_generation_id,
                target_protection,
                ..
            } => {
                if !valid_id(operation_id)
                    || !valid_id(owner_device_id)
                    || *started_at_ms < 0
                    || lease_expires_at_ms <= started_at_ms
                    || !valid_id(target_generation_id)
                    || target_generation_id == &self.identity.generation_id
                {
                    return Err(AppError::InvalidData(
                        "remote vault freeze state is invalid".into(),
                    ));
                }
                target_protection.validate()?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionedVaultIdentity {
    pub identity: VaultIdentity,
    pub protection: VaultProtection,
    pub compatibility: Option<VaultCompatibility>,
    pub state: VaultState,
    pub etag: String,
}

impl VersionedVaultIdentity {
    pub fn document(&self) -> VaultDocument {
        VaultDocument {
            identity: self.identity.clone(),
            protection: self.protection.clone(),
            compatibility: self.compatibility,
            state: self.state.clone(),
        }
    }

    pub fn active_document(&self) -> VaultDocument {
        VaultDocument {
            identity: self.identity.clone(),
            protection: self.protection.clone(),
            compatibility: self.compatibility,
            state: VaultState::Active,
        }
    }

    pub fn released_v1_compatibility_active(&self) -> bool {
        self.compatibility == Some(VaultCompatibility::ReleasedV1Writers)
    }
}

impl VaultIdentity {
    fn validate(&self) -> Result<()> {
        if self.format_version != VAULT_FORMAT_VERSION
            || !valid_id(&self.vault_id)
            || !valid_id(&self.generation_id)
        {
            return Err(AppError::InvalidData(
                "remote vault identity is invalid".into(),
            ));
        }
        Ok(())
    }
}

pub async fn load_or_create_identity<B: CloudBackend + ?Sized>(
    backend: &B,
    proposed: VaultIdentity,
) -> Result<VaultIdentity> {
    Ok(load_or_create_vault(
        backend,
        VaultDocument::active(proposed, VaultProtection::plain()),
    )
    .await?
    .identity)
}

pub async fn load_or_create_vault<B: CloudBackend + ?Sized>(
    backend: &B,
    proposed: VaultDocument,
) -> Result<VaultDocument> {
    proposed.validate()?;
    let (root, path) = identity_paths()?;
    backend
        .create_collection(&root)
        .await
        .map_err(cloud_error)?;
    match backend.get(&path).await {
        Ok(object) => decode_document(&object.bytes),
        Err(error) if error.kind() == "not_found" => {
            let bytes = serde_json::to_vec(&proposed)?;
            match backend.put_if_absent(&path, &bytes).await {
                Ok(()) => {}
                Err(error) if error.kind() == "precondition" => {}
                Err(error) => return Err(cloud_error(error)),
            }
            let stored = backend.get(&path).await.map_err(cloud_error)?;
            decode_document(&stored.bytes)
        }
        Err(error) => Err(cloud_error(error)),
    }
}

pub async fn load_versioned_identity<B: CloudBackend + ?Sized>(
    backend: &B,
) -> Result<VersionedVaultIdentity> {
    let (_root, path) = identity_paths()?;
    let object = backend.get(&path).await.map_err(cloud_error)?;
    let document = decode_document(&object.bytes)?;
    let etag = object
        .etag
        .ok_or_else(|| AppError::InvalidData("remote vault identity has no ETag".into()))?;
    Ok(VersionedVaultIdentity {
        identity: document.identity,
        protection: document.protection,
        compatibility: document.compatibility,
        state: document.state,
        etag,
    })
}

pub async fn replace_identity<B: CloudBackend + ?Sized>(
    backend: &B,
    expected: &VaultIdentity,
    replacement: VaultIdentity,
) -> Result<VersionedVaultIdentity> {
    expected.validate()?;
    replacement.validate()?;
    let current = load_versioned_identity(backend).await?;
    if current.identity != *expected || current.state != VaultState::Active {
        return Err(AppError::InvalidData(
            "remote vault identity changed during maintenance".into(),
        ));
    }
    compare_and_swap_document(
        backend,
        &current,
        VaultDocument {
            identity: replacement,
            protection: current.protection.clone(),
            compatibility: current.compatibility,
            state: VaultState::Active,
        },
    )
    .await
}

pub async fn begin_generation_freeze<B: CloudBackend + ?Sized>(
    backend: &B,
    expected: &VaultDocument,
    target_generation_id: &str,
    target_protection: VaultProtection,
    operation_id: &str,
) -> Result<VersionedVaultIdentity> {
    let started_at_ms = current_time_millis()?;
    begin_generation_freeze_owned(
        backend,
        expected,
        target_generation_id,
        target_protection,
        operation_id,
        "legacy-maintenance",
        started_at_ms,
        started_at_ms.saturating_add(DEFAULT_MAINTENANCE_LEASE_MS),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn begin_generation_freeze_owned<B: CloudBackend + ?Sized>(
    backend: &B,
    expected: &VaultDocument,
    target_generation_id: &str,
    target_protection: VaultProtection,
    operation_id: &str,
    owner_device_id: &str,
    started_at_ms: i64,
    lease_expires_at_ms: i64,
) -> Result<VersionedVaultIdentity> {
    begin_generation_freeze_owned_with_policy(
        backend,
        expected,
        target_generation_id,
        target_protection,
        operation_id,
        owner_device_id,
        started_at_ms,
        lease_expires_at_ms,
        false,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn begin_generation_freeze_owned_with_policy<B: CloudBackend + ?Sized>(
    backend: &B,
    expected: &VaultDocument,
    target_generation_id: &str,
    target_protection: VaultProtection,
    operation_id: &str,
    owner_device_id: &str,
    started_at_ms: i64,
    lease_expires_at_ms: i64,
    retire_released_v1_compatibility: bool,
) -> Result<VersionedVaultIdentity> {
    expected.validate()?;
    if expected.state != VaultState::Active
        || !valid_id(target_generation_id)
        || target_generation_id == expected.identity.generation_id
        || !valid_id(operation_id)
        || !valid_id(owner_device_id)
        || started_at_ms < 0
        || lease_expires_at_ms <= started_at_ms
    {
        return Err(AppError::InvalidData(
            "generation freeze request is invalid".into(),
        ));
    }
    target_protection.validate()?;
    let current = load_versioned_identity(backend).await?;
    if current.document() != *expected {
        return Err(AppError::InvalidData(
            "remote vault identity changed before generation freeze".into(),
        ));
    }
    let mut frozen = expected.clone();
    frozen.state = VaultState::Frozen {
        operation_id: operation_id.to_owned(),
        owner_device_id: owner_device_id.to_owned(),
        started_at_ms,
        lease_expires_at_ms,
        target_generation_id: target_generation_id.to_owned(),
        target_protection,
        stage: GenerationMaintenanceStage::BuildingBaseline,
        retire_released_v1_compatibility,
    };
    compare_and_swap_document(backend, &current, frozen).await
}

pub async fn mark_frozen_generation_ready<B: CloudBackend + ?Sized>(
    backend: &B,
    frozen: &VersionedVaultIdentity,
) -> Result<VersionedVaultIdentity> {
    let mut replacement = frozen.document();
    match &mut replacement.state {
        VaultState::Frozen {
            stage,
            lease_expires_at_ms,
            ..
        } => {
            *stage = GenerationMaintenanceStage::ReadyToActivate;
            *lease_expires_at_ms =
                current_time_millis()?.saturating_add(DEFAULT_MAINTENANCE_LEASE_MS);
        }
        _ => {
            return Err(AppError::InvalidData(
                "remote vault is not frozen for generation activation".into(),
            ));
        }
    }
    compare_and_swap_document(backend, frozen, replacement).await
}

pub async fn begin_head_publish<B: CloudBackend + ?Sized>(
    backend: &B,
    expected: &VersionedVaultIdentity,
    request: HeadPublishRequest,
) -> Result<VersionedVaultIdentity> {
    let expected_document = expected.document();
    expected_document.validate()?;
    if expected.state != VaultState::Active {
        return Err(AppError::InvalidData(
            "remote vault is not active for publication".into(),
        ));
    }
    let mut publishing = expected_document;
    publishing.state = VaultState::Publishing {
        operation_id: request.operation_id,
        owner_device_id: request.owner_device_id,
        started_at_ms: request.started_at_ms,
        lease_expires_at_ms: request.lease_expires_at_ms,
        head_path: request.head_path,
        expected_head_etag: request.expected_head_etag,
        replacement_head_json: request.replacement_head_json,
        published_mutation_count: request.published_mutation_count,
    };
    compare_and_swap_document(backend, expected, publishing).await
}

pub async fn recover_head_publish<B: CloudBackend + ?Sized>(
    backend: &B,
    publishing: &VersionedVaultIdentity,
) -> Result<VersionedVaultIdentity> {
    let (head_path, expected_head_etag, replacement_head_json) = match &publishing.state {
        VaultState::Publishing {
            head_path,
            expected_head_etag,
            replacement_head_json,
            ..
        } => (
            RemotePath::parse(head_path)
                .map_err(|_| AppError::InvalidData("remote head path is invalid".into()))?,
            expected_head_etag.clone(),
            replacement_head_json.clone(),
        ),
        _ => {
            return Err(AppError::InvalidData(
                "remote vault is not publishing a device head".into(),
            ));
        }
    };
    let replacement_bytes = replacement_head_json.as_bytes();
    let write = match expected_head_etag.as_deref() {
        Some(etag) => {
            backend
                .put_if_match(&head_path, replacement_bytes, etag)
                .await
        }
        None => backend.put_if_absent(&head_path, replacement_bytes).await,
    };
    match write {
        Ok(()) => {}
        Err(error) if error.kind() == "precondition" => {
            let stored = backend.get(&head_path).await.map_err(cloud_error)?;
            if stored.bytes != replacement_bytes {
                return Err(AppError::InvalidData(
                    "remote device head changed during publication recovery".into(),
                ));
            }
        }
        Err(error) => return Err(cloud_error(error)),
    }
    let stored_head = backend.get(&head_path).await.map_err(cloud_error)?;
    if stored_head.bytes != replacement_bytes {
        return Err(AppError::InvalidData(
            "remote device head update could not be verified".into(),
        ));
    }
    compare_and_swap_document(backend, publishing, publishing.active_document()).await
}

pub async fn activate_frozen_generation<B: CloudBackend + ?Sized>(
    backend: &B,
    frozen: &VersionedVaultIdentity,
) -> Result<VersionedVaultIdentity> {
    let (target_generation_id, target_protection, retire_released_v1_compatibility) =
        match &frozen.state {
            VaultState::Frozen {
                target_generation_id,
                target_protection,
                stage: GenerationMaintenanceStage::ReadyToActivate,
                retire_released_v1_compatibility,
                ..
            } => (
                target_generation_id.clone(),
                target_protection.clone(),
                *retire_released_v1_compatibility,
            ),
            VaultState::Frozen { .. } => {
                return Err(AppError::InvalidData(
                    "remote generation baseline is not ready for activation".into(),
                ));
            }
            _ => {
                return Err(AppError::InvalidData(
                    "remote vault is not frozen for generation activation".into(),
                ));
            }
        };
    let replacement = VaultDocument {
        identity: VaultIdentity {
            generation_id: target_generation_id,
            ..frozen.identity.clone()
        },
        protection: target_protection,
        compatibility: if retire_released_v1_compatibility {
            None
        } else {
            frozen.compatibility
        },
        state: VaultState::Active,
    };
    compare_and_swap_document(backend, frozen, replacement).await
}

pub async fn activate_frozen_generation_outcome<B: CloudBackend + ?Sized>(
    backend: &B,
    frozen: &VersionedVaultIdentity,
) -> VaultUpdateOutcome {
    let (target_generation_id, target_protection, retire_released_v1_compatibility) =
        match &frozen.state {
            VaultState::Frozen {
                target_generation_id,
                target_protection,
                stage: GenerationMaintenanceStage::ReadyToActivate,
                retire_released_v1_compatibility,
                ..
            } => (
                target_generation_id.clone(),
                target_protection.clone(),
                *retire_released_v1_compatibility,
            ),
            VaultState::Frozen { .. } => {
                return VaultUpdateOutcome::Unknown(AppError::InvalidData(
                    "remote generation baseline is not ready for activation".into(),
                ));
            }
            _ => {
                return VaultUpdateOutcome::Unknown(AppError::InvalidData(
                    "remote vault is not frozen for generation activation".into(),
                ));
            }
        };
    let replacement = VaultDocument {
        identity: VaultIdentity {
            generation_id: target_generation_id,
            ..frozen.identity.clone()
        },
        protection: target_protection,
        compatibility: if retire_released_v1_compatibility {
            None
        } else {
            frozen.compatibility
        },
        state: VaultState::Active,
    };
    compare_and_swap_document_outcome(backend, frozen, replacement).await
}

pub async fn recover_frozen_generation<B: CloudBackend + ?Sized>(
    backend: &B,
) -> Result<VersionedVaultIdentity> {
    recover_frozen_generation_at(backend, current_time_millis()?).await
}

pub async fn recover_frozen_generation_from_snapshot<B: CloudBackend + ?Sized>(
    backend: &B,
    current: &VersionedVaultIdentity,
) -> Result<VersionedVaultIdentity> {
    recover_frozen_generation_from_snapshot_at(backend, current, current_time_millis()?).await
}

pub async fn recover_frozen_generation_at<B: CloudBackend + ?Sized>(
    backend: &B,
    now_ms: i64,
) -> Result<VersionedVaultIdentity> {
    let current = load_versioned_identity(backend).await?;
    recover_frozen_generation_from_snapshot_at(backend, &current, now_ms).await
}

async fn recover_frozen_generation_from_snapshot_at<B: CloudBackend + ?Sized>(
    backend: &B,
    current: &VersionedVaultIdentity,
    now_ms: i64,
) -> Result<VersionedVaultIdentity> {
    if current.state == VaultState::Active {
        return Ok(current.clone());
    }
    let lease_expires_at_ms = match &current.state {
        VaultState::Frozen {
            lease_expires_at_ms,
            ..
        } => *lease_expires_at_ms,
        VaultState::Publishing { .. } => {
            return recover_head_publish(backend, current).await;
        }
        VaultState::Active => unreachable!(),
    };
    if now_ms < lease_expires_at_ms {
        return Err(AppError::InvalidData(
            "remote vault maintenance is active".into(),
        ));
    }
    let replacement = current.active_document();
    compare_and_swap_document(backend, current, replacement).await
}

pub async fn rollback_frozen_generation<B: CloudBackend + ?Sized>(
    backend: &B,
    frozen: &VersionedVaultIdentity,
) -> Result<VersionedVaultIdentity> {
    let current = load_versioned_identity(backend).await?;
    if current.document() == frozen.document() {
        let replacement = frozen.active_document();
        return compare_and_swap_document(backend, &current, replacement).await;
    }
    let target_generation = match &frozen.state {
        VaultState::Frozen {
            target_generation_id,
            ..
        } => target_generation_id.as_str(),
        _ => {
            return Err(AppError::InvalidData(
                "generation rollback requires a frozen vault".into(),
            ));
        }
    };
    if current.state == VaultState::Active
        && current.identity.vault_id == frozen.identity.vault_id
        && (current.identity.generation_id == frozen.identity.generation_id
            || current.identity.generation_id == target_generation)
    {
        return Ok(current);
    }
    Err(AppError::InvalidData(
        "remote vault changed during generation rollback".into(),
    ))
}

async fn compare_and_swap_document<B: CloudBackend + ?Sized>(
    backend: &B,
    expected: &VersionedVaultIdentity,
    replacement: VaultDocument,
) -> Result<VersionedVaultIdentity> {
    match compare_and_swap_document_outcome(backend, expected, replacement).await {
        VaultUpdateOutcome::Committed(stored) => Ok(stored),
        VaultUpdateOutcome::Rejected { error, .. } => Err(error),
        VaultUpdateOutcome::Unknown(error) => Err(error),
    }
}

async fn compare_and_swap_document_outcome<B: CloudBackend + ?Sized>(
    backend: &B,
    expected: &VersionedVaultIdentity,
    replacement: VaultDocument,
) -> VaultUpdateOutcome {
    if let Err(error) = replacement.validate() {
        return VaultUpdateOutcome::Unknown(error);
    }
    let (_root, path) = match identity_paths() {
        Ok(paths) => paths,
        Err(error) => return VaultUpdateOutcome::Unknown(error),
    };
    let bytes = match serde_json::to_vec(&replacement) {
        Ok(bytes) => bytes,
        Err(error) => return VaultUpdateOutcome::Unknown(error.into()),
    };
    let write = backend.put_if_match(&path, &bytes, &expected.etag).await;
    let mut last_read_error = None;
    for _ in 0..3 {
        match load_versioned_identity(backend).await {
            Ok(stored) if stored.document() == replacement => {
                return VaultUpdateOutcome::Committed(stored);
            }
            Ok(stored) if stored.document() == expected.document() => {
                return VaultUpdateOutcome::Rejected {
                    current: stored,
                    error: match &write {
                        Err(error) => cloud_error(error.clone()),
                        Ok(()) => {
                            AppError::InvalidData("remote vault update did not take effect".into())
                        }
                    },
                };
            }
            Ok(_) => {
                return VaultUpdateOutcome::Unknown(AppError::InvalidData(
                    "remote vault identity changed during conditional update".into(),
                ));
            }
            Err(error) => last_read_error = Some(error),
        }
    }
    VaultUpdateOutcome::Unknown(match write {
        Err(error) => cloud_error(error),
        Ok(()) => last_read_error.unwrap_or_else(|| {
            AppError::InvalidData("remote vault update outcome is unknown".into())
        }),
    })
}

fn identity_paths() -> Result<(RemotePath, RemotePath)> {
    let root = RemotePath::parse("v1").map_err(|error| AppError::InvalidData(error.to_string()))?;
    let path = root
        .join("vault.json")
        .map_err(|error| AppError::InvalidData(error.to_string()))?;
    Ok((root, path))
}

fn decode_document(bytes: &[u8]) -> Result<VaultDocument> {
    let document: VaultDocument = serde_json::from_slice(bytes)?;
    document.validate()?;
    Ok(document)
}

fn verifier_associated_data(vault_id: &str) -> Vec<u8> {
    let mut data = b"ai-chat-memory/v2/vault-verifier\0".to_vec();
    data.extend_from_slice(vault_id.as_bytes());
    data
}

fn current_time_millis() -> Result<i64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AppError::InvalidData("system clock predates Unix epoch".into()))?
        .as_millis();
    i64::try_from(millis)
        .map_err(|_| AppError::InvalidData("system clock value is too large".into()))
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn cloud_error(error: CloudError) -> AppError {
    AppError::Cloud(error)
}

#[cfg(test)]
mod tests {
    use super::{
        GenerationMaintenanceStage, VaultDocument, VaultIdentity, VaultProtection, VaultState,
        VaultUpdateOutcome, activate_frozen_generation, activate_frozen_generation_outcome,
        begin_generation_freeze, begin_generation_freeze_owned, decode_document,
        load_or_create_identity, load_or_create_vault, load_versioned_identity,
        mark_frozen_generation_ready, recover_frozen_generation, recover_frozen_generation_at,
        replace_identity,
    };
    use crate::{
        error::AppError,
        models::S3CloudSyncSettings,
        sync::{
            backend::{
                CloudBackend, CloudError, CloudErrorKind, CloudResult, RemoteEntry, RemoteObject,
                RemotePath,
            },
            crypto::Argon2idConfig,
            s3::S3Backend,
            test_s3_server::TestS3,
        },
    };
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct FailFirstVaultReadAfterWriteBackend {
        inner: S3Backend,
        fail_confirmation_read: AtomicBool,
    }

    #[async_trait]
    impl CloudBackend for FailFirstVaultReadAfterWriteBackend {
        async fn list_depth_one(&self, path: &RemotePath) -> CloudResult<Vec<RemoteEntry>> {
            self.inner.list_depth_one(path).await
        }

        async fn create_collection(&self, path: &RemotePath) -> CloudResult<()> {
            self.inner.create_collection(path).await
        }

        async fn get(&self, path: &RemotePath) -> CloudResult<RemoteObject> {
            if path.display() == "v1/vault.json"
                && self.fail_confirmation_read.swap(false, Ordering::SeqCst)
            {
                return Err(CloudError::new(
                    CloudErrorKind::Offline,
                    "injected confirmation read failure",
                ));
            }
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
            let result = self.inner.put_if_match(path, bytes, etag).await;
            if result.is_ok() && path.display() == "v1/vault.json" {
                self.fail_confirmation_read.store(true, Ordering::SeqCst);
            }
            result
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

    fn proposed(suffix: &str) -> VaultIdentity {
        VaultIdentity {
            format_version: 2,
            vault_id: format!("vault-{suffix}"),
            generation_id: format!("generation-{suffix}"),
        }
    }

    fn backend(server: &TestS3) -> S3Backend {
        S3Backend::new(
            &S3CloudSyncSettings {
                endpoint_url: server.endpoint().into(),
                region: "us-east-1".into(),
                bucket: "archive".into(),
                prefix: "shared-vault".into(),
                force_path_style: true,
            },
            "AKID",
            "secret-key",
            None,
        )
        .unwrap()
    }

    #[test]
    fn released_v1_compatibility_has_a_stable_wire_value() {
        let document = VaultDocument::released_v1_compatible(VaultIdentity {
            format_version: 2,
            vault_id: "default".into(),
            generation_id: "generation-1".into(),
        });

        let wire = serde_json::to_value(&document).unwrap();

        assert_eq!(wire["compatibility"], "released_v1_writers");
        assert_eq!(
            decode_document(&serde_json::to_vec(&wire).unwrap()).unwrap(),
            document
        );
    }

    #[test]
    fn vault_json_without_compatibility_defaults_to_none() {
        let document = VaultDocument::active(proposed("old-writer"), VaultProtection::plain());
        let wire = serde_json::to_value(&document).unwrap();

        assert!(wire.get("compatibility").is_none());
        assert_eq!(
            decode_document(&serde_json::to_vec(&wire).unwrap()).unwrap(),
            document
        );
    }

    #[test]
    fn frozen_vault_json_without_retirement_flag_defaults_to_false() {
        let wire = serde_json::json!({
            "format_version": 2,
            "vault_id": "vault-old-writer",
            "generation_id": "generation-old-writer",
            "protection": { "algorithm": "plain" },
            "state": {
                "status": "frozen",
                "operation_id": "rotation-old-writer",
                "owner_device_id": "device-old-writer",
                "started_at_ms": 1,
                "lease_expires_at_ms": 2,
                "target_generation_id": "generation-next",
                "target_protection": { "algorithm": "plain" },
                "stage": "building_baseline"
            }
        });

        let document = decode_document(&serde_json::to_vec(&wire).unwrap()).unwrap();

        assert!(document.compatibility.is_none());
        assert!(matches!(
            document.state,
            VaultState::Frozen {
                stage: GenerationMaintenanceStage::BuildingBaseline,
                retire_released_v1_compatibility: false,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn later_devices_adopt_the_first_remote_identity() {
        let server = TestS3::start("AKID", None).await;
        let first_backend = backend(&server);
        let second_backend = backend(&server);

        let first = load_or_create_identity(&first_backend, proposed("first"))
            .await
            .unwrap();
        let second = load_or_create_identity(&second_backend, proposed("second"))
            .await
            .unwrap();

        assert_eq!(first, proposed("first"));
        assert_eq!(second, first);
    }

    #[tokio::test]
    async fn generation_switch_is_conditional_and_rejects_a_stale_identity() {
        let server = TestS3::start("AKID", None).await;
        let backend = backend(&server);
        let first = proposed("first");
        load_or_create_identity(&backend, first.clone())
            .await
            .unwrap();
        let replacement = VaultIdentity {
            generation_id: "generation-next".into(),
            ..first.clone()
        };

        let switched = replace_identity(&backend, &first, replacement.clone())
            .await
            .unwrap();

        assert_eq!(switched.identity, replacement);
        assert!(
            replace_identity(&backend, &first, proposed("other"))
                .await
                .is_err()
        );
    }

    #[test]
    fn encrypted_vault_metadata_verifies_the_candidate_passphrase() {
        let protection = VaultProtection::encrypted("vault-encrypted", "correct horse").unwrap();

        assert!(
            protection
                .derive_protector("vault-encrypted", "correct horse")
                .unwrap()
                .is_some()
        );
        assert!(
            protection
                .derive_protector("vault-encrypted", "wrong horse")
                .is_err()
        );
        assert!(
            protection
                .derive_protector("another-vault", "correct horse")
                .is_err()
        );
        let wire = serde_json::to_value(&protection).unwrap();
        assert_eq!(wire["algorithm"], "x_cha_cha20_poly1305");
        assert_eq!(wire["kdf"]["algorithm"], "argon2id");
        assert_eq!(wire["kdf"]["version"], 1);
        assert_eq!(wire["kdf"]["memory_kib"], 64 * 1024);
        assert!(wire["verifier_hex"].as_str().unwrap().len() >= 64);
    }

    #[test]
    fn encrypted_vaults_use_fresh_random_salts() {
        let first = VaultProtection::encrypted("vault-encrypted", "correct horse").unwrap();
        let second = VaultProtection::encrypted("vault-encrypted", "correct horse").unwrap();

        assert_ne!(
            first.kdf.as_ref().unwrap().salt_hex,
            second.kdf.as_ref().unwrap().salt_hex
        );
    }

    #[test]
    fn remote_kdf_metadata_rejects_excessive_parallelism() {
        let mut protection = VaultProtection::encrypted_with_config(
            "vault-encrypted",
            "correct horse",
            Argon2idConfig {
                salt: [9; 16],
                memory_kib: 8 * 1024,
                iterations: 1,
                parallelism: 1,
            },
        )
        .unwrap();
        protection.kdf.as_mut().unwrap().parallelism = 5;

        assert!(matches!(
            protection.validate(),
            Err(crate::error::AppError::InvalidData(_))
        ));
    }

    #[tokio::test]
    async fn existing_vault_adopts_the_first_protection_policy() {
        let server = TestS3::start("AKID", None).await;
        let backend = backend(&server);
        let first = VaultDocument::active(
            proposed("first"),
            VaultProtection::encrypted("vault-first", "shared passphrase").unwrap(),
        );
        let second = VaultDocument::active(proposed("second"), VaultProtection::plain());

        let stored = load_or_create_vault(&backend, first.clone()).await.unwrap();
        let adopted = load_or_create_vault(&backend, second).await.unwrap();

        assert_eq!(stored, first);
        assert_eq!(adopted, first);
        assert!(
            adopted
                .protection
                .derive_protector(&adopted.identity.vault_id, "shared passphrase")
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn frozen_generation_can_activate_only_while_its_cas_token_is_current() {
        let server = TestS3::start("AKID", None).await;
        let backend = backend(&server);
        let active = VaultDocument::active(proposed("first"), VaultProtection::plain());
        load_or_create_vault(&backend, active.clone())
            .await
            .unwrap();
        let target = VaultProtection::encrypted("vault-first", "next passphrase").unwrap();

        let frozen = begin_generation_freeze(
            &backend,
            &active,
            "generation-next",
            target.clone(),
            "rotation-1",
        )
        .await
        .unwrap();
        assert!(matches!(
            frozen.state,
            VaultState::Frozen {
                ref operation_id,
                ref target_generation_id,
                ..
            } if operation_id == "rotation-1" && target_generation_id == "generation-next"
        ));

        let recovered = recover_frozen_generation_at(&backend, i64::MAX)
            .await
            .unwrap();
        assert_eq!(recovered.document(), active);
        assert!(activate_frozen_generation(&backend, &frozen).await.is_err());
        assert_eq!(
            load_versioned_identity(&backend).await.unwrap().document(),
            active
        );
    }

    #[tokio::test]
    async fn fresh_frozen_generation_is_not_recovered_by_another_client() {
        let server = TestS3::start("AKID", None).await;
        let backend = backend(&server);
        let active = VaultDocument::active(proposed("first"), VaultProtection::plain());
        load_or_create_vault(&backend, active.clone())
            .await
            .unwrap();
        begin_generation_freeze(
            &backend,
            &active,
            "generation-next",
            VaultProtection::plain(),
            "rotation-fresh",
        )
        .await
        .unwrap();
        let path = RemotePath::parse("v1/vault.json").unwrap();
        let before = backend.get(&path).await.unwrap();
        let wire: serde_json::Value = serde_json::from_slice(&before.bytes).unwrap();

        assert!(wire["state"]["owner_device_id"].is_string());
        assert!(wire["state"]["started_at_ms"].is_number());
        assert!(wire["state"]["lease_expires_at_ms"].is_number());
        assert!(wire["state"]["stage"].is_string());
        assert!(recover_frozen_generation(&backend).await.is_err());

        let after = backend.get(&path).await.unwrap();
        assert_eq!(after.bytes, before.bytes);
        assert_eq!(after.etag, before.etag);
    }

    #[tokio::test]
    async fn owned_generation_freeze_uses_the_caller_provided_lease_window() {
        // ENG-11：租约窗口必须来自调用方的单一时间源。owned 变体不得内部再取
        // current_time_millis()，否则调用方计算的 lease 边界与冻结实际起始时间错位。
        let server = TestS3::start("AKID", None).await;
        let backend = backend(&server);
        let active = VaultDocument::active(proposed("lease"), VaultProtection::plain());
        load_or_create_vault(&backend, active.clone())
            .await
            .unwrap();

        begin_generation_freeze_owned(
            &backend,
            &active,
            "generation-next",
            VaultProtection::plain(),
            "rotation-lease",
            "device-lease",
            1_000,
            2_000,
        )
        .await
        .unwrap();

        let frozen = load_versioned_identity(&backend).await.unwrap();
        match frozen.state {
            VaultState::Frozen {
                operation_id,
                owner_device_id,
                started_at_ms,
                lease_expires_at_ms,
                target_generation_id,
                ..
            } => {
                assert_eq!(operation_id, "rotation-lease");
                assert_eq!(owner_device_id, "device-lease");
                assert_eq!(target_generation_id, "generation-next");
                assert_eq!(
                    (started_at_ms, lease_expires_at_ms),
                    (1_000, 2_000),
                    "the freeze must record the caller-provided lease window verbatim"
                );
            }
            other => panic!("expected a frozen vault state, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activation_reconciles_a_successful_put_after_confirmation_get_fails_once() {
        let server = TestS3::start("AKID", None).await;
        let inner = backend(&server);
        let active = VaultDocument::active(proposed("first"), VaultProtection::plain());
        load_or_create_vault(&inner, active.clone()).await.unwrap();
        let frozen = begin_generation_freeze(
            &inner,
            &active,
            "generation-next",
            VaultProtection::plain(),
            "rotation-confirmation",
        )
        .await
        .unwrap();
        let frozen = mark_frozen_generation_ready(&inner, &frozen).await.unwrap();
        let backend = FailFirstVaultReadAfterWriteBackend {
            inner,
            fail_confirmation_read: AtomicBool::new(false),
        };

        let activated = activate_frozen_generation(&backend, &frozen).await.unwrap();

        assert_eq!(activated.identity.generation_id, "generation-next");
        assert_eq!(activated.state, VaultState::Active);
    }

    #[tokio::test]
    async fn frozen_generation_activation_commits_target_identity_and_protection() {
        let server = TestS3::start("AKID", None).await;
        let backend = backend(&server);
        let active = VaultDocument::active(proposed("first"), VaultProtection::plain());
        load_or_create_vault(&backend, active.clone())
            .await
            .unwrap();
        let target = VaultProtection::encrypted("vault-first", "next passphrase").unwrap();
        let frozen = begin_generation_freeze(
            &backend,
            &active,
            "generation-next",
            target.clone(),
            "rotation-1",
        )
        .await
        .unwrap();
        let frozen = mark_frozen_generation_ready(&backend, &frozen)
            .await
            .unwrap();

        let activated = activate_frozen_generation(&backend, &frozen).await.unwrap();

        assert_eq!(activated.identity.vault_id, "vault-first");
        assert_eq!(activated.identity.generation_id, "generation-next");
        assert_eq!(activated.protection, target);
        assert_eq!(activated.state, VaultState::Active);
    }

    #[tokio::test]
    async fn frozen_generation_cannot_activate_before_the_baseline_is_ready() {
        let server = TestS3::start("AKID", None).await;
        let backend = backend(&server);
        let active = VaultDocument::active(proposed("first"), VaultProtection::plain());
        load_or_create_vault(&backend, active.clone())
            .await
            .unwrap();
        let frozen = begin_generation_freeze(
            &backend,
            &active,
            "generation-next",
            VaultProtection::plain(),
            "rotation-not-ready",
        )
        .await
        .unwrap();
        let before = backend
            .get(&RemotePath::parse("v1/vault.json").unwrap())
            .await
            .unwrap();

        assert!(activate_frozen_generation(&backend, &frozen).await.is_err());
        assert!(matches!(
            activate_frozen_generation_outcome(&backend, &frozen).await,
            VaultUpdateOutcome::Unknown(AppError::InvalidData(_))
        ));

        let after = backend
            .get(&RemotePath::parse("v1/vault.json").unwrap())
            .await
            .unwrap();
        assert_eq!(after.bytes, before.bytes);
        assert_eq!(after.etag, before.etag);
    }
}
