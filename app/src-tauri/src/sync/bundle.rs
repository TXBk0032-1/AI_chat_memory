use crate::{
    error::{AppError, Result},
    sync::crypto::PayloadProtector,
    sync::types::{
        BundleChange, BundleContents, EntityKey, EntityVersion, MutationOperation,
        NormalizedSessionSnapshot,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    io::{Cursor, Read},
};

const MAGIC: &[u8; 4] = b"ACMB";
const ENVELOPE_VERSION: u8 = 1;
const PREFIX_LENGTH: usize = 9;
const LIMIT_ERROR_PREFIX: &str = "bundle exceeds configured limits: ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviousValidation {
    StrictChain,
    ReleasedV1Unchained,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompressionAlgorithm {
    Zstandard,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProtectionAlgorithm {
    Plain,
    XChaCha20Poly1305,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BundleHeader {
    pub vault_id: String,
    pub generation_id: String,
    pub device_id: String,
    pub start_seq: i64,
    pub end_seq: i64,
    pub previous_path: Option<String>,
    pub previous_sha256: Option<String>,
    pub previous_end_seq: Option<i64>,
    pub compression: CompressionAlgorithm,
    pub protection: ProtectionAlgorithm,
    pub nonce: Option<String>,
    pub payload_length: u64,
    pub payload_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedBundle {
    pub bytes: Vec<u8>,
    pub file_sha256: String,
    pub header: BundleHeader,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedBundle {
    pub header: BundleHeader,
    pub contents: BundleContents,
}

#[derive(Debug, Clone)]
pub struct BundleLimits {
    pub max_envelope_bytes: usize,
    pub max_header_bytes: usize,
    pub max_decompressed_bytes: usize,
    pub max_entries: usize,
    pub max_file_bytes: usize,
    pub max_ndjson_line_bytes: usize,
}

impl Default for BundleLimits {
    fn default() -> Self {
        Self {
            max_envelope_bytes: 128 * 1024 * 1024,
            max_header_bytes: 1024 * 1024,
            max_decompressed_bytes: 512 * 1024 * 1024,
            max_entries: 2_000,
            max_file_bytes: 128 * 1024 * 1024,
            max_ndjson_line_bytes: 8 * 1024 * 1024,
        }
    }
}

impl BundleLimits {
    #[cfg(test)]
    fn test() -> Self {
        Self {
            max_envelope_bytes: 4 * 1024 * 1024,
            max_header_bytes: 64 * 1024,
            max_decompressed_bytes: 4 * 1024 * 1024,
            max_entries: 100,
            max_file_bytes: 1024 * 1024,
            max_ndjson_line_bytes: 64 * 1024,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleManifest {
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleChangeWire {
    local_seq: i64,
    key: EntityKey,
    operation: MutationOperation,
    version: EntityVersion,
    content_hash: Option<String>,
}

#[derive(Serialize)]
struct AuthenticatedHeader<'a> {
    vault_id: &'a str,
    generation_id: &'a str,
    device_id: &'a str,
    start_seq: i64,
    end_seq: i64,
    previous_path: Option<&'a str>,
    previous_sha256: Option<&'a str>,
    previous_end_seq: Option<i64>,
    compression: CompressionAlgorithm,
    protection: ProtectionAlgorithm,
    nonce: Option<&'a str>,
}

pub fn seal_bundle(contents: &BundleContents) -> Result<SealedBundle> {
    seal_bundle_with_limits(contents, &BundleLimits::default())
}

pub fn seal_bundle_with_limits(
    contents: &BundleContents,
    limits: &BundleLimits,
) -> Result<SealedBundle> {
    let payload = build_payload(contents)?;
    let sealed = seal_payload(contents, payload, ProtectionAlgorithm::Plain, None)?;
    validate_sealed_bundle(&sealed, limits, None)?;
    Ok(sealed)
}

pub fn seal_bundle_protected(
    contents: &BundleContents,
    protector: &dyn PayloadProtector,
    nonce: [u8; 24],
) -> Result<SealedBundle> {
    seal_bundle_protected_with_limits(contents, protector, nonce, &BundleLimits::default())
}

pub fn seal_bundle_protected_with_limits(
    contents: &BundleContents,
    protector: &dyn PayloadProtector,
    nonce: [u8; 24],
    limits: &BundleLimits,
) -> Result<SealedBundle> {
    if protector.algorithm() == ProtectionAlgorithm::Plain {
        return Err(AppError::Crypto(
            "encrypted bundle requires a non-plain protector".into(),
        ));
    }
    let plaintext = build_payload(contents)?;
    let nonce_hex = hex::encode(nonce);
    let aad =
        authenticated_header_bytes(contents, protector.algorithm(), Some(nonce_hex.as_str()))?;
    let payload = protector.seal(&aad, &plaintext, nonce)?;
    let sealed = seal_payload(contents, payload, protector.algorithm(), Some(nonce_hex))?;
    validate_sealed_bundle(&sealed, limits, Some(protector))?;
    Ok(sealed)
}

fn build_payload(contents: &BundleContents) -> Result<Vec<u8>> {
    validate_contents(contents)?;
    let mut changes = Vec::new();
    let mut sessions = HashMap::new();
    for change in &contents.changes {
        let wire = BundleChangeWire {
            local_seq: change.local_seq,
            key: change.key.clone(),
            operation: change.operation.clone(),
            version: change.version.clone(),
            content_hash: change.content_hash.clone(),
        };
        serde_json::to_writer(&mut changes, &wire)?;
        changes.push(b'\n');
        if let (Some(hash), Some(snapshot)) = (&change.content_hash, &change.snapshot) {
            sessions
                .entry(hash.clone())
                .or_insert(serde_json::to_vec(snapshot)?);
        }
    }
    let manifest = BundleManifest {
        vault_id: contents.vault_id.clone(),
        generation_id: contents.generation_id.clone(),
        device_id: contents.device_id.clone(),
        start_seq: contents.start_seq,
        end_seq: contents.end_seq,
        previous_path: contents.previous_path.clone(),
        previous_sha256: contents.previous_sha256.clone(),
        previous_end_seq: contents.previous_end_seq,
        change_count: contents.changes.len(),
        changes_sha256: sha256_hex(&changes),
    };
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        append_tar_file(&mut builder, "bundle.json", &serde_json::to_vec(&manifest)?)?;
        append_tar_file(&mut builder, "changes.ndjson", &changes)?;
        let mut sessions = sessions.into_iter().collect::<Vec<_>>();
        sessions.sort_by(|left, right| left.0.cmp(&right.0));
        for (hash, bytes) in sessions {
            append_tar_file(&mut builder, &format!("sessions/{hash}.json"), &bytes)?;
        }
        builder.finish()?;
    }
    Ok(zstd::stream::encode_all(Cursor::new(tar_bytes), 3)?)
}

pub fn open_bundle(bytes: &[u8], limits: &BundleLimits) -> Result<DecodedBundle> {
    open_bundle_protected(bytes, limits, None)
}

pub fn open_bundle_protected(
    bytes: &[u8],
    limits: &BundleLimits,
    protector: Option<&dyn PayloadProtector>,
) -> Result<DecodedBundle> {
    open_bundle_protected_with_previous_validation(
        bytes,
        limits,
        protector,
        PreviousValidation::StrictChain,
    )
}

pub(crate) fn open_released_v1_unchained_bundle_protected(
    bytes: &[u8],
    limits: &BundleLimits,
    protector: Option<&dyn PayloadProtector>,
) -> Result<DecodedBundle> {
    open_bundle_protected_with_previous_validation(
        bytes,
        limits,
        protector,
        PreviousValidation::ReleasedV1Unchained,
    )
}

fn open_bundle_protected_with_previous_validation(
    bytes: &[u8],
    limits: &BundleLimits,
    protector: Option<&dyn PayloadProtector>,
    previous_validation: PreviousValidation,
) -> Result<DecodedBundle> {
    if bytes.len() > limits.max_envelope_bytes || bytes.len() < PREFIX_LENGTH {
        return invalid("bundle envelope size is invalid");
    }
    if &bytes[..4] != MAGIC || bytes[4] != ENVELOPE_VERSION {
        return invalid("unsupported bundle envelope");
    }
    let header_length = u32::from_be_bytes(bytes[5..9].try_into().unwrap()) as usize;
    if header_length > limits.max_header_bytes {
        return invalid("bundle header exceeds limit");
    }
    let payload_offset = PREFIX_LENGTH
        .checked_add(header_length)
        .ok_or_else(|| AppError::InvalidData("bundle header length overflow".into()))?;
    if payload_offset > bytes.len() {
        return invalid("bundle header is truncated");
    }
    let header: BundleHeader = serde_json::from_slice(&bytes[PREFIX_LENGTH..payload_offset])
        .map_err(|error| AppError::InvalidData(format!("invalid bundle header: {error}")))?;
    validate_header(&header, previous_validation)?;
    let expected_protection = protector
        .map(PayloadProtector::algorithm)
        .unwrap_or(ProtectionAlgorithm::Plain);
    if header.protection != expected_protection {
        return Err(AppError::Crypto(
            "bundle protection does not match the expected vault policy".into(),
        ));
    }
    let payload = &bytes[payload_offset..];
    if usize::try_from(header.payload_length).ok() != Some(payload.len())
        || sha256_hex(payload) != header.payload_sha256
    {
        return invalid("bundle payload length or digest mismatch");
    }
    let plaintext = match header.protection {
        ProtectionAlgorithm::Plain => payload.to_vec(),
        ProtectionAlgorithm::XChaCha20Poly1305 => {
            let protector = protector.ok_or_else(|| {
                AppError::Crypto("encrypted bundle requires a payload protector".into())
            })?;
            if protector.algorithm() != header.protection {
                return Err(AppError::Crypto(
                    "payload protector algorithm mismatch".into(),
                ));
            }
            let nonce = decode_nonce(header.nonce.as_deref())?;
            let contents = BundleContents {
                vault_id: header.vault_id.clone(),
                generation_id: header.generation_id.clone(),
                device_id: header.device_id.clone(),
                start_seq: header.start_seq,
                end_seq: header.end_seq,
                previous_path: header.previous_path.clone(),
                previous_sha256: header.previous_sha256.clone(),
                previous_end_seq: header.previous_end_seq,
                changes: Vec::new(),
            };
            let aad =
                authenticated_header_bytes(&contents, header.protection, header.nonce.as_deref())?;
            protector.open(&aad, payload, nonce)?
        }
    };
    let decompressed = decompress_limited(&plaintext, limits.max_decompressed_bytes)?;
    let files = read_tar(&decompressed, limits)?;
    decode_contents(header, files, limits, previous_validation)
}

fn validate_sealed_bundle(
    sealed: &SealedBundle,
    limits: &BundleLimits,
    protector: Option<&dyn PayloadProtector>,
) -> Result<()> {
    open_bundle_protected(&sealed.bytes, limits, protector)
        .map(|_| ())
        .map_err(|error| match error {
            AppError::InvalidData(message) => {
                AppError::InvalidData(format!("{LIMIT_ERROR_PREFIX}{message}"))
            }
            other => other,
        })
}

pub(crate) fn is_bundle_limit_error(error: &AppError) -> bool {
    matches!(
        error,
        AppError::InvalidData(message) if message.starts_with(LIMIT_ERROR_PREFIX)
    )
}

fn validate_contents(contents: &BundleContents) -> Result<()> {
    validate_contents_with_previous_validation(contents, PreviousValidation::StrictChain)
}

fn validate_contents_with_previous_validation(
    contents: &BundleContents,
    previous_validation: PreviousValidation,
) -> Result<()> {
    if contents.vault_id.is_empty()
        || contents.generation_id.is_empty()
        || contents.device_id.is_empty()
        || contents.start_seq < 0
        || contents.end_seq < contents.start_seq
    {
        return invalid("bundle identity or sequence range is invalid");
    }
    validate_previous(
        contents.start_seq,
        contents.previous_path.as_deref(),
        contents.previous_sha256.as_deref(),
        contents.previous_end_seq,
        previous_validation,
    )?;
    if contents.changes.is_empty()
        || contents.changes.last().map(|change| change.local_seq) != Some(contents.end_seq)
    {
        return invalid("bundle changes do not match the sequence range");
    }
    let mut sequences = HashSet::new();
    let mut previous_seq = None;
    for change in &contents.changes {
        if change.local_seq < contents.start_seq
            || change.local_seq > contents.end_seq
            || !sequences.insert(change.local_seq)
            || previous_seq.is_some_and(|previous| previous >= change.local_seq)
        {
            return invalid("bundle change sequence is invalid");
        }
        previous_seq = Some(change.local_seq);
        match change.operation {
            MutationOperation::Upsert => {
                let (Some(hash), Some(snapshot)) = (&change.content_hash, &change.snapshot) else {
                    return invalid("upsert requires content hash and snapshot");
                };
                if snapshot.key != change.key
                    || !is_sha256(hash)
                    || sha256_hex(&serde_json::to_vec(snapshot)?) != *hash
                {
                    return invalid("upsert snapshot does not match its key or hash");
                }
            }
            MutationOperation::Delete => {
                if change.content_hash.is_some() || change.snapshot.is_some() {
                    return invalid("delete must not carry content");
                }
            }
        }
    }
    Ok(())
}

fn validate_header(header: &BundleHeader, previous_validation: PreviousValidation) -> Result<()> {
    if header.vault_id.is_empty()
        || header.generation_id.is_empty()
        || header.device_id.is_empty()
        || header.start_seq < 0
        || header.end_seq < header.start_seq
        || !is_sha256(&header.payload_sha256)
    {
        return invalid("bundle header fields are invalid");
    }
    match header.protection {
        ProtectionAlgorithm::Plain if header.nonce.is_none() => {}
        ProtectionAlgorithm::XChaCha20Poly1305
            if header.nonce.as_deref().is_some_and(|value| {
                value.len() == 48 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
            }) => {}
        _ => return invalid("bundle protection and nonce do not match"),
    }
    validate_previous(
        header.start_seq,
        header.previous_path.as_deref(),
        header.previous_sha256.as_deref(),
        header.previous_end_seq,
        previous_validation,
    )?;
    Ok(())
}

fn validate_previous(
    start_seq: i64,
    path: Option<&str>,
    sha256: Option<&str>,
    end_seq: Option<i64>,
    validation: PreviousValidation,
) -> Result<()> {
    match (path, sha256, end_seq) {
        (None, None, None)
            if start_seq == 1
                || (validation == PreviousValidation::ReleasedV1Unchained && start_seq > 1) =>
        {
            Ok(())
        }
        (Some(path), Some(sha256), Some(end_seq))
            if validation == PreviousValidation::StrictChain
                && is_safe_relative_path(path)
                && path.ends_with(".acmb")
                && is_sha256(sha256)
                && end_seq >= 0
                && end_seq.checked_add(1) == Some(start_seq) =>
        {
            Ok(())
        }
        _ => invalid("previous bundle chain fields are invalid"),
    }
}

fn seal_payload(
    contents: &BundleContents,
    payload: Vec<u8>,
    protection: ProtectionAlgorithm,
    nonce: Option<String>,
) -> Result<SealedBundle> {
    let header = BundleHeader {
        vault_id: contents.vault_id.clone(),
        generation_id: contents.generation_id.clone(),
        device_id: contents.device_id.clone(),
        start_seq: contents.start_seq,
        end_seq: contents.end_seq,
        previous_path: contents.previous_path.clone(),
        previous_sha256: contents.previous_sha256.clone(),
        previous_end_seq: contents.previous_end_seq,
        compression: CompressionAlgorithm::Zstandard,
        protection,
        nonce,
        payload_length: payload.len() as u64,
        payload_sha256: sha256_hex(&payload),
    };
    let header_bytes = serde_json::to_vec(&header)?;
    let header_length = u32::try_from(header_bytes.len())
        .map_err(|_| AppError::InvalidData("bundle header is too large".into()))?;
    let mut bytes = Vec::with_capacity(PREFIX_LENGTH + header_bytes.len() + payload.len());
    bytes.extend_from_slice(MAGIC);
    bytes.push(ENVELOPE_VERSION);
    bytes.extend_from_slice(&header_length.to_be_bytes());
    bytes.extend_from_slice(&header_bytes);
    bytes.extend_from_slice(&payload);
    Ok(SealedBundle {
        file_sha256: sha256_hex(&bytes),
        bytes,
        header,
    })
}

fn authenticated_header_bytes(
    contents: &BundleContents,
    protection: ProtectionAlgorithm,
    nonce: Option<&str>,
) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&AuthenticatedHeader {
        vault_id: &contents.vault_id,
        generation_id: &contents.generation_id,
        device_id: &contents.device_id,
        start_seq: contents.start_seq,
        end_seq: contents.end_seq,
        previous_path: contents.previous_path.as_deref(),
        previous_sha256: contents.previous_sha256.as_deref(),
        previous_end_seq: contents.previous_end_seq,
        compression: CompressionAlgorithm::Zstandard,
        protection,
        nonce,
    })?)
}

fn decode_nonce(value: Option<&str>) -> Result<[u8; 24]> {
    let bytes = hex::decode(value.ok_or_else(|| AppError::InvalidData("nonce is missing".into()))?)
        .map_err(|_| AppError::InvalidData("nonce is not valid hex".into()))?;
    bytes
        .try_into()
        .map_err(|_| AppError::InvalidData("nonce must contain 24 bytes".into()))
}

fn append_tar_file<W: std::io::Write>(
    builder: &mut tar::Builder<W>,
    path: &str,
    bytes: &[u8],
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o600);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(bytes.len() as u64);
    header.set_cksum();
    builder.append_data(&mut header, path, Cursor::new(bytes))?;
    Ok(())
}

fn decompress_limited(payload: &[u8], limit: usize) -> Result<Vec<u8>> {
    let decoder = zstd::stream::read::Decoder::new(Cursor::new(payload))?;
    let mut output = Vec::new();
    decoder
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut output)?;
    if output.len() > limit {
        return invalid("decompressed bundle exceeds limit");
    }
    Ok(output)
}

fn read_tar(bytes: &[u8], limits: &BundleLimits) -> Result<HashMap<String, Vec<u8>>> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let mut files = HashMap::new();
    for (index, entry) in archive.entries()?.enumerate() {
        if index >= limits.max_entries {
            return invalid("bundle contains too many entries");
        }
        let entry = entry?;
        if !entry.header().entry_type().is_file() {
            return invalid("bundle contains a non-regular entry");
        }
        let path = entry
            .path_bytes()
            .as_ref()
            .strip_suffix(&[0])
            .unwrap_or(entry.path_bytes().as_ref())
            .to_vec();
        let path = std::str::from_utf8(&path)
            .map_err(|_| AppError::InvalidData("bundle path is not UTF-8".into()))?
            .to_owned();
        validate_tar_path(&path)?;
        if entry.size() > limits.max_file_bytes as u64 || files.contains_key(&path) {
            return invalid("bundle entry is oversized or duplicated");
        }
        let mut data = Vec::new();
        entry
            .take(limits.max_file_bytes.saturating_add(1) as u64)
            .read_to_end(&mut data)?;
        if data.len() > limits.max_file_bytes {
            return invalid("bundle entry exceeds file limit");
        }
        files.insert(path, data);
    }
    Ok(files)
}

fn validate_tar_path(path: &str) -> Result<()> {
    let parts = path.split('/').collect::<Vec<_>>();
    if !is_safe_relative_path(path) {
        return invalid("bundle contains an unsafe path");
    }
    let allowed = path == "bundle.json"
        || path == "changes.ndjson"
        || (parts.len() == 2
            && parts[0] == "sessions"
            && parts[1].ends_with(".json")
            && is_sha256(&parts[1][..parts[1].len() - 5]));
    if !allowed {
        return invalid("bundle contains an unknown path");
    }
    Ok(())
}

fn is_safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains(':')
        && !path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}

fn decode_contents(
    header: BundleHeader,
    mut files: HashMap<String, Vec<u8>>,
    limits: &BundleLimits,
    previous_validation: PreviousValidation,
) -> Result<DecodedBundle> {
    let manifest_bytes = files
        .remove("bundle.json")
        .ok_or_else(|| AppError::InvalidData("bundle manifest is missing".into()))?;
    let changes_bytes = files
        .remove("changes.ndjson")
        .ok_or_else(|| AppError::InvalidData("bundle changes are missing".into()))?;
    let manifest: BundleManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| AppError::InvalidData(format!("invalid bundle manifest: {error}")))?;
    if manifest.vault_id != header.vault_id
        || manifest.generation_id != header.generation_id
        || manifest.device_id != header.device_id
        || manifest.start_seq != header.start_seq
        || manifest.end_seq != header.end_seq
        || manifest.previous_path != header.previous_path
        || manifest.previous_sha256 != header.previous_sha256
        || manifest.previous_end_seq != header.previous_end_seq
        || manifest.changes_sha256 != sha256_hex(&changes_bytes)
    {
        return invalid("bundle manifest does not match envelope");
    }
    let wires = parse_changes(&changes_bytes, limits.max_ndjson_line_bytes)?;
    if wires.len() != manifest.change_count {
        return invalid("bundle change count does not match manifest");
    }
    let mut changes = Vec::with_capacity(wires.len());
    for wire in wires {
        let snapshot = match wire.operation {
            MutationOperation::Upsert => {
                let hash = wire.content_hash.as_deref().ok_or_else(|| {
                    AppError::InvalidData("upsert content hash is missing".into())
                })?;
                let path = format!("sessions/{hash}.json");
                let snapshot_bytes = files.get(&path).ok_or_else(|| {
                    AppError::InvalidData("upsert snapshot file is missing".into())
                })?;
                if sha256_hex(snapshot_bytes) != hash {
                    return invalid("upsert snapshot hash mismatch");
                }
                Some(
                    serde_json::from_slice::<NormalizedSessionSnapshot>(snapshot_bytes).map_err(
                        |error| AppError::InvalidData(format!("invalid session snapshot: {error}")),
                    )?,
                )
            }
            MutationOperation::Delete => {
                if wire.content_hash.is_some() {
                    return invalid("delete carries a content hash");
                }
                None
            }
        };
        changes.push(BundleChange {
            local_seq: wire.local_seq,
            key: wire.key,
            operation: wire.operation,
            version: wire.version,
            content_hash: wire.content_hash,
            snapshot,
        });
    }
    let contents = BundleContents {
        vault_id: header.vault_id.clone(),
        generation_id: header.generation_id.clone(),
        device_id: header.device_id.clone(),
        start_seq: header.start_seq,
        end_seq: header.end_seq,
        previous_path: header.previous_path.clone(),
        previous_sha256: header.previous_sha256.clone(),
        previous_end_seq: header.previous_end_seq,
        changes,
    };
    validate_contents_with_previous_validation(&contents, previous_validation)?;
    Ok(DecodedBundle { header, contents })
}

fn parse_changes(bytes: &[u8], max_line: usize) -> Result<Vec<BundleChangeWire>> {
    let mut changes = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        if line.len() > max_line {
            return invalid("bundle NDJSON line exceeds limit");
        }
        changes.push(
            serde_json::from_slice(line).map_err(|error| {
                AppError::InvalidData(format!("invalid bundle change: {error}"))
            })?,
        );
    }
    Ok(changes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn invalid<T>(message: &str) -> Result<T> {
    Err(AppError::InvalidData(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::crypto::{Argon2idConfig, XChaChaProtector};
    use crate::sync::types::{
        BundleChange, BundleContents, EntityKey, EntityVersion, MutationOperation,
        NormalizedSessionSnapshot,
    };
    use serde_json::json;
    use sha2::{Digest, Sha256};

    fn fixture() -> BundleContents {
        let snapshot = NormalizedSessionSnapshot {
            key: EntityKey {
                platform: "chat".into(),
                platform_session_id: "session-1".into(),
            },
            title: "Bundle fixture".into(),
            created_at: None,
            updated_at: None,
            imported_at: "2026-07-29T00:00:00Z".into(),
            raw_data: json!({"fixture": true}),
            messages: vec![],
        };
        let snapshot_bytes = serde_json::to_vec(&snapshot).unwrap();
        BundleContents {
            vault_id: "vault-a".into(),
            generation_id: "generation-a".into(),
            device_id: "device-a".into(),
            start_seq: 7,
            end_seq: 7,
            previous_path: Some("devices/device-a/bundles/1-6-old.acmb".into()),
            previous_sha256: Some("ab".repeat(32)),
            previous_end_seq: Some(6),
            changes: vec![BundleChange {
                local_seq: 7,
                key: snapshot.key.clone(),
                operation: MutationOperation::Upsert,
                version: EntityVersion::new(123, 0, "device-a"),
                content_hash: Some(hex::encode(Sha256::digest(snapshot_bytes))),
                snapshot: Some(snapshot),
            }],
        }
    }

    #[test]
    fn plaintext_bundle_round_trips_and_preserves_chain() {
        let expected = fixture();
        let sealed = seal_bundle(&expected).unwrap();
        let decoded = open_bundle(&sealed.bytes, &BundleLimits::test()).unwrap();

        assert_eq!(decoded.contents, expected);
        assert_eq!(decoded.header.previous_end_seq, Some(6));
        assert_eq!(
            hex::encode(Sha256::digest(&sealed.bytes)),
            sealed.file_sha256
        );
    }

    #[test]
    fn encrypted_bundle_round_trips_and_authenticates_header() {
        let protector = XChaChaProtector::derive(
            "sync passphrase",
            &Argon2idConfig {
                salt: [4; 16],
                memory_kib: 8 * 1024,
                iterations: 2,
                parallelism: 1,
            },
        )
        .unwrap();
        let sealed = seal_bundle_protected(&fixture(), &protector, [6; 24]).unwrap();
        assert_eq!(
            sealed.header.protection,
            ProtectionAlgorithm::XChaCha20Poly1305
        );
        assert_eq!(
            open_bundle_protected(&sealed.bytes, &BundleLimits::test(), Some(&protector))
                .unwrap()
                .contents,
            fixture()
        );
        assert!(open_bundle(&sealed.bytes, &BundleLimits::test()).is_err());

        let mut tampered = sealed.bytes.clone();
        let header_length = u32::from_be_bytes(tampered[5..9].try_into().unwrap()) as usize;
        let header = &mut tampered[PREFIX_LENGTH..PREFIX_LENGTH + header_length];
        let offset = header
            .windows(b"vault-a".len())
            .position(|window| window == b"vault-a")
            .unwrap();
        header[offset..offset + b"vault-a".len()].copy_from_slice(b"vault-b");
        assert!(open_bundle_protected(&tampered, &BundleLimits::test(), Some(&protector)).is_err());
    }

    #[test]
    fn protected_reader_rejects_a_plain_bundle() {
        let protector = XChaChaProtector::derive(
            "sync passphrase",
            &Argon2idConfig {
                salt: [4; 16],
                memory_kib: 8 * 1024,
                iterations: 2,
                parallelism: 1,
            },
        )
        .unwrap();
        let sealed = seal_bundle(&fixture()).unwrap();

        let error = open_bundle_protected(&sealed.bytes, &BundleLimits::test(), Some(&protector))
            .unwrap_err();

        assert!(matches!(error, AppError::Crypto(_)), "{error:?}");
    }

    #[test]
    fn bounded_seal_rejects_every_reader_limit_before_publication() {
        let contents = fixture();
        let sealed = seal_bundle(&contents).unwrap();

        let mut envelope_limits = BundleLimits::test();
        envelope_limits.max_envelope_bytes = sealed.bytes.len() - 1;
        assert!(seal_bundle_with_limits(&contents, &envelope_limits).is_err());

        let mut decompressed_limits = BundleLimits::test();
        decompressed_limits.max_decompressed_bytes = 16;
        assert!(seal_bundle_with_limits(&contents, &decompressed_limits).is_err());

        let mut entry_limits = BundleLimits::test();
        entry_limits.max_entries = 2;
        assert!(seal_bundle_with_limits(&contents, &entry_limits).is_err());

        let mut file_limits = BundleLimits::test();
        file_limits.max_file_bytes = 8;
        assert!(seal_bundle_with_limits(&contents, &file_limits).is_err());

        let mut line_limits = BundleLimits::test();
        line_limits.max_ndjson_line_bytes = 8;
        assert!(seal_bundle_with_limits(&contents, &line_limits).is_err());
    }

    #[test]
    fn rejects_unknown_major_version_and_final_size_limit() {
        let sealed = seal_bundle(&fixture()).unwrap();
        let mut unknown = sealed.bytes.clone();
        unknown[4] = 2;
        assert!(open_bundle(&unknown, &BundleLimits::test()).is_err());

        let mut limits = BundleLimits::test();
        limits.max_envelope_bytes = sealed.bytes.len() - 1;
        assert!(open_bundle(&sealed.bytes, &limits).is_err());
    }

    #[test]
    fn rejects_invalid_previous_bundle_chain_fields() {
        let mut contents = fixture();
        contents.previous_sha256 = Some("not-a-sha256".into());
        assert!(seal_bundle(&contents).is_err());
    }

    #[test]
    fn rejects_changes_outside_exact_strict_sequence_order() {
        let mut contents = fixture();
        let mut second = contents.changes[0].clone();
        second.local_seq = 8;
        contents.end_seq = 8;
        contents.changes.push(second);
        contents.changes.swap(0, 1);

        assert!(seal_bundle(&contents).is_err());
    }

    #[test]
    fn rejects_traversal_symlink_duplicate_and_decompressed_limit() {
        for invalid in [
            InvalidArchiveFixture::Traversal,
            InvalidArchiveFixture::Symlink,
            InvalidArchiveFixture::Duplicate,
        ] {
            let bytes = invalid_archive_fixture(&fixture(), invalid).unwrap();
            assert!(open_bundle(&bytes, &BundleLimits::test()).is_err());
        }

        let sealed = seal_bundle(&fixture()).unwrap();
        let mut limits = BundleLimits::test();
        limits.max_decompressed_bytes = 16;
        assert!(open_bundle(&sealed.bytes, &limits).is_err());
    }

    #[test]
    fn enforces_entry_file_and_ndjson_line_limits() {
        let sealed = seal_bundle(&fixture()).unwrap();

        let mut entry_limits = BundleLimits::test();
        entry_limits.max_entries = 2;
        assert!(open_bundle(&sealed.bytes, &entry_limits).is_err());

        let mut file_limits = BundleLimits::test();
        file_limits.max_file_bytes = 8;
        assert!(open_bundle(&sealed.bytes, &file_limits).is_err());

        let mut line_limits = BundleLimits::test();
        line_limits.max_ndjson_line_bytes = 8;
        assert!(open_bundle(&sealed.bytes, &line_limits).is_err());
    }

    #[derive(Clone, Copy)]
    enum InvalidArchiveFixture {
        Traversal,
        Symlink,
        Duplicate,
    }

    fn invalid_archive_fixture(
        contents: &BundleContents,
        fixture: InvalidArchiveFixture,
    ) -> Result<Vec<u8>> {
        let mut tar = Vec::new();
        match fixture {
            InvalidArchiveFixture::Traversal => {
                append_raw_tar_entry(&mut tar, "../escape", b"x", b'0')
            }
            InvalidArchiveFixture::Symlink => {
                append_raw_tar_entry(&mut tar, "bundle.json", b"", b'2')
            }
            InvalidArchiveFixture::Duplicate => {
                append_raw_tar_entry(&mut tar, "bundle.json", b"{}", b'0');
                append_raw_tar_entry(&mut tar, "bundle.json", b"{}", b'0');
            }
        }
        tar.extend_from_slice(&[0; 1024]);
        let payload = zstd::stream::encode_all(Cursor::new(tar), 3)?;
        Ok(seal_payload(contents, payload, ProtectionAlgorithm::Plain, None)?.bytes)
    }

    fn append_raw_tar_entry(output: &mut Vec<u8>, path: &str, data: &[u8], kind: u8) {
        let mut header = [0_u8; 512];
        header[..path.len()].copy_from_slice(path.as_bytes());
        write_octal(&mut header[100..108], 0o600);
        write_octal(&mut header[108..116], 0);
        write_octal(&mut header[116..124], 0);
        write_octal(&mut header[124..136], data.len() as u64);
        write_octal(&mut header[136..148], 0);
        header[148..156].fill(b' ');
        header[156] = kind;
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum = header.iter().map(|byte| *byte as u64).sum();
        write_octal(&mut header[148..156], checksum);
        output.extend_from_slice(&header);
        output.extend_from_slice(data);
        output.resize(output.len().next_multiple_of(512), 0);
    }

    fn write_octal(field: &mut [u8], value: u64) {
        field.fill(b'0');
        let digits = format!("{:o}", value);
        let start = field.len() - digits.len() - 1;
        field[start..start + digits.len()].copy_from_slice(digits.as_bytes());
        field[field.len() - 1] = 0;
    }
}
