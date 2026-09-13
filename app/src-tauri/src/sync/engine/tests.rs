use super::*;
use crate::{
    database::{
        connection::{initialize_schema, register_sqlite_vec},
        import_sessions,
    },
    models::NormalizedSession,
    sync::{
        backend::{CloudError, CloudErrorKind, CloudResult, RemoteEntry, RemoteObject},
        bundle::{
            BundleHeader, CompressionAlgorithm, ProtectionAlgorithm, SealedBundle, open_bundle,
            open_bundle_protected, seal_bundle,
        },
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

    async fn put_if_match(&self, path: &RemotePath, bytes: &[u8], etag: &str) -> CloudResult<()> {
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

    async fn put_if_match(&self, path: &RemotePath, bytes: &[u8], etag: &str) -> CloudResult<()> {
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
        path.display().ends_with("/head.json") && self.fail_head_write.swap(false, Ordering::SeqCst)
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

    async fn put_if_match(&self, path: &RemotePath, bytes: &[u8], etag: &str) -> CloudResult<()> {
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

/// Counts `get` calls for bundle objects so tests can observe exactly how
/// many remote bundles were downloaded and decoded.
struct CountingBundleGetBackend {
    inner: Arc<S3Backend>,
    bundle_gets: AtomicUsize,
}

#[async_trait]
impl CloudBackend for CountingBundleGetBackend {
    async fn list_depth_one(&self, path: &RemotePath) -> CloudResult<Vec<RemoteEntry>> {
        self.inner.list_depth_one(path).await
    }

    async fn create_collection(&self, path: &RemotePath) -> CloudResult<()> {
        self.inner.create_collection(path).await
    }

    async fn get(&self, path: &RemotePath) -> CloudResult<RemoteObject> {
        if path.display().contains("/bundles/") {
            self.bundle_gets.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.get(path).await
    }

    async fn put_immutable(&self, path: &RemotePath, bytes: &[u8]) -> CloudResult<()> {
        self.inner.put_immutable(path, bytes).await
    }

    async fn put_if_match(&self, path: &RemotePath, bytes: &[u8], etag: &str) -> CloudResult<()> {
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

fn append_released_bundle_file(builder: &mut tar::Builder<&mut Vec<u8>>, path: &str, bytes: &[u8]) {
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
        if wire.version.wall_ms < 0 || wire.version.counter < 0 || wire.version.device_id.is_empty()
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
                let snapshot_bytes = files
                    .get(&format!("sessions/{hash}.json"))
                    .ok_or_else(|| AppError::InvalidData("released snapshot is missing".into()))?;
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
    crate::test_support::test_s3_backend(server, "engine-tests")
}

fn test_protector(passphrase: &str) -> Arc<dyn PayloadProtector> {
    crate::test_support::test_protector("vault", passphrase)
}

fn test_protection(passphrase: &str) -> VaultProtection {
    crate::test_support::test_protection("vault", passphrase)
}

async fn initialize_test_vault<B: CloudBackend + ?Sized>(
    backend: &B,
    vault_id: &str,
    generation_id: &str,
    protection: VaultProtection,
) {
    crate::test_support::initialize_test_vault(backend, vault_id, generation_id, protection).await;
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
        RemotePath::parse("v1/generations/generation-1/devices/device-upgraded/head.json").unwrap();
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
    let first =
        seal_released_v1_unchained_bundle(&released_v1_contents(2, 2, "released-starts-at-two"));
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
                &RemotePath::parse("v1/generations/generation-1/devices/device-upgraded/head.json")
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

#[tokio::test]
async fn released_v1_reconstruction_skips_duplicate_bundle_objects_by_sha256() {
    let server = TestS3::start("AKID", None).await;
    let backend = Arc::new(CountingBundleGetBackend {
        inner: s3_backend(&server),
        bundle_gets: AtomicUsize::new(0),
    });
    initialize_released_v1_test_vault(backend.as_ref()).await;
    let first = publish_released_v1_bundle(
        backend.as_ref(),
        &released_v1_contents(1, 1, "released-one"),
    )
    .await;
    publish_released_v1_bundle(
        backend.as_ref(),
        &released_v1_contents(2, 2, "released-two"),
    )
    .await;
    // 恶意副本：与 1-1 同内容（同 sha256）的对象挂在另一个序列区间名下。
    let duplicate_path = RemotePath::parse(&format!(
        "v1/generations/generation-1/devices/device-old/bundles/2-2-{}.acmb",
        first.sha256
    ))
    .unwrap();
    let duplicate_bytes = backend
        .get(&RemotePath::parse(&first.path).unwrap())
        .await
        .unwrap();
    backend
        .put_immutable(&duplicate_path, &duplicate_bytes.bytes)
        .await
        .unwrap();

    let (store, pool) = test_store("device-upgraded").await;
    let engine = SyncEngine::new(
        store,
        backend.clone(),
        "default",
        "generation-1",
        "device-upgraded",
    );
    // 只统计 engine 在重建阶段发起的 bundle 下载，不包含测试自身读取副本字节。
    backend.bundle_gets.store(0, Ordering::SeqCst);

    let report = engine.run_once(SyncTrigger::Manual).await.unwrap();

    // 同名内容只下载解码一次，重复对象在收集阶段按 sha256 跳过，
    // 而不是下载后才因序列区间不符而报错。
    assert_eq!(report.pulled, 2);
    let titles: Vec<String> = sqlx::query_scalar("SELECT title FROM sessions ORDER BY title")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(titles, vec!["released-one", "released-two"]);
    assert_eq!(
        backend.bundle_gets.load(Ordering::SeqCst),
        3,
        "head boundary download + two unique candidates; the duplicate copy must be skipped before download"
    );
}

#[test]
fn released_v1_compatibility_reader_does_not_weaken_strict_bundle_validation() {
    let sealed = seal_released_v1_unchained_bundle(&released_v1_contents(2, 2, "released-two"));

    let strict_error = open_bundle(&sealed.bytes, &BundleLimits::default()).unwrap_err();
    assert!(
        matches!(strict_error, AppError::InvalidData(ref message) if message.contains("previous bundle chain fields are invalid")),
        "{strict_error:?}"
    );
    let compatible =
        open_released_v1_unchained_bundle_protected(&sealed.bytes, &BundleLimits::default(), None)
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
    let deleted_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE platform_session_id = 'remote-0'")
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

    let sessions_a: Vec<(String, String, String, String, String, String, String)> = sqlx::query_as(
        "SELECT platform, platform_session_id, COALESCE(title, ''),
                        COALESCE(created_at, ''), COALESCE(updated_at, ''),
                        COALESCE(imported_at, ''), COALESCE(raw_data, '')
                 FROM sessions ORDER BY platform, platform_session_id",
    )
    .fetch_all(&pool_a)
    .await
    .unwrap();
    let sessions_b: Vec<(String, String, String, String, String, String, String)> = sqlx::query_as(
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
async fn encrypted_generation_discards_legacy_plain_staged_bundle_and_reseals() {
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

    // The staged bundle is plain while the generation is encrypted, so the
    // staged bytes cannot be opened with the current protector. Instead of
    // pinning the publish loop against the unrecoverable staged row forever,
    // the engine discards it and reseals the legitimate pending outbox with
    // the correct protector.
    let report = engine.publish_pending().await.unwrap();

    assert_eq!(report.published, pending.len());
    // The remote head now points at the freshly-sealed encrypted bundle, not
    // the discarded plain one.
    let head_bytes = backend
        .get(&engine.head_path().unwrap())
        .await
        .unwrap()
        .bytes;
    let head: HeadDocument = serde_json::from_slice(&head_bytes).unwrap();
    assert_ne!(head.sha256, sealed.file_sha256);
    // All pending mutations have been acknowledged and cleared from the outbox.
    assert!(store.pending_mutations(10).await.unwrap().is_empty());
    // The orphaned staged row has been cleaned up: no stage='staged'
    // rows remain for the legacy plain bundle.
    let staged_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sync_published_bundles WHERE stage = 'staged'")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(staged_count, 0);
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
    // The mismatched staged row is discarded so the next publish
    // attempt reseals fresh instead of retrying against a prefix that will
    // never match.
    let staged_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sync_published_bundles WHERE stage = 'staged'")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(staged_count, 0);
    // A retry now succeeds: the outbox is intact and no stale staged row blocks it.
    let report = engine.publish_pending().await.unwrap();
    assert_eq!(report.published, pending.len());
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
    // 恢复不再提前返回。本次运行没有发布新 bundle，只是把崩溃残留的
    // head 推进补完并把该变更幂等确认为 acknowledge，published 如实为 0。
    assert_eq!((retry.published, retry.acknowledged), (0, 1));
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

    // 恢复不再提前返回——staged 前缀经 head 幂等确认为 acknowledge，
    // 重启期间新增的变更在同一轮直接发布，不再被跳过到下一次 run。
    assert_eq!((retry.published, retry.acknowledged), (1, 2));
    let final_objects = backend.bundle_objects().await;
    assert_eq!(final_objects.len(), 2);
    assert_eq!(
        final_objects.get(&first_path),
        Some(&first_bytes),
        "staged bundle must be recovered in place, not resealed"
    );
    let head: HeadDocument = serde_json::from_slice(
        &backend
            .get(&restarted_engine.head_path().unwrap())
            .await
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(head.end_seq, 2);
    assert_ne!(head.path, first_path);
    assert!(store.pending_mutations(10).await.unwrap().is_empty());
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

    // 恢复后同一轮继续发布——被合并出 outbox 的 staged 变更经 head 幂等
    // 确认为 acknowledge，合并后的新变更立即发布，staged bundle 原位复用不重封。
    assert_eq!((recovered.published, recovered.acknowledged), (1, 1));
    assert_eq!(backend.bundle_objects().await.len(), 2);
    assert_eq!(
        backend
            .get(&RemotePath::parse(&first_path).unwrap())
            .await
            .unwrap()
            .bytes,
        first_bytes,
        "staged bundle must be recovered in place, not resealed"
    );
    let head: HeadDocument = serde_json::from_slice(
        &backend
            .get(&restarted_engine.head_path().unwrap())
            .await
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(head.end_seq, 2);
    assert!(store.pending_mutations(10).await.unwrap().is_empty());

    let next = restarted_engine.publish_pending().await.unwrap();
    assert_eq!(next, SyncReport::default());
    assert_eq!(backend.bundle_objects().await.len(), 2);
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
async fn publish_pending_continues_publishing_after_recovering_a_stuck_head_publish() {
    let server = TestS3::start("AKID", None).await;
    let backend = s3_backend(&server);
    let (store, _pool) = test_store("device-a").await;
    store
        .queue_local_upsert(snapshot(0, "crashed-bundle"), 1_000)
        .await
        .unwrap();
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
    engine.run_once(SyncTrigger::Manual).await.unwrap();
    assert!(store.pending_mutations(10).await.unwrap().is_empty());

    // 模拟崩溃残留：vault 卡在 Publishing（历史 published_mutation_count=5），head 已推进。
    let identity = load_versioned_identity(backend.as_ref()).await.unwrap();
    let head = backend.get(&engine.head_path().unwrap()).await.unwrap();
    begin_head_publish(
        backend.as_ref(),
        &identity,
        HeadPublishRequest {
            operation_id: "publish-stuck".into(),
            owner_device_id: "device-a".into(),
            started_at_ms: current_time_millis(),
            lease_expires_at_ms: current_time_millis().saturating_add(DEFAULT_MAINTENANCE_LEASE_MS),
            head_path: engine.head_path().unwrap().display(),
            expected_head_etag: None,
            replacement_head_json: String::from_utf8(head.bytes).unwrap(),
            published_mutation_count: 5,
        },
    )
    .await
    .unwrap();

    // 恢复期间新产生的 pending 变更。
    store
        .queue_local_upsert(snapshot(1, "fresh-mutation"), 2_000)
        .await
        .unwrap();

    let report = engine.publish_pending().await.unwrap();
    assert_eq!(
        report.published, 1,
        "must publish the real new mutation instead of the stale recovered count"
    );
    assert!(
        store.pending_mutations(10).await.unwrap().is_empty(),
        "pending work must not be skipped until the next run"
    );
}

#[tokio::test]
async fn run_once_does_not_inflate_published_with_recovered_publication_counts() {
    let server = TestS3::start("AKID", None).await;
    let backend = s3_backend(&server);
    let (store, _pool) = test_store("device-a").await;
    store
        .queue_local_upsert(snapshot(0, "already-published"), 1_000)
        .await
        .unwrap();
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
    engine.run_once(SyncTrigger::Manual).await.unwrap();

    // 模拟崩溃残留：vault 卡在 Publishing，历史 published_mutation_count=5
    // 是请求时的值，与本次 run 实际发布的条数无关。
    let identity = load_versioned_identity(backend.as_ref()).await.unwrap();
    let head = backend.get(&engine.head_path().unwrap()).await.unwrap();
    begin_head_publish(
        backend.as_ref(),
        &identity,
        HeadPublishRequest {
            operation_id: "publish-stuck".into(),
            owner_device_id: "device-a".into(),
            started_at_ms: current_time_millis(),
            lease_expires_at_ms: current_time_millis().saturating_add(DEFAULT_MAINTENANCE_LEASE_MS),
            head_path: engine.head_path().unwrap().display(),
            expected_head_etag: None,
            replacement_head_json: String::from_utf8(head.bytes).unwrap(),
            published_mutation_count: 5,
        },
    )
    .await
    .unwrap();
    store
        .queue_local_upsert(snapshot(1, "fresh-mutation"), 2_000)
        .await
        .unwrap();

    // 恢复计数是历史请求值，恢复本身不发布，不得叠加进 published。
    let report = engine.run_once(SyncTrigger::Manual).await.unwrap();
    assert_eq!(
        (report.published, report.acknowledged),
        (1, 1),
        "published must only count the fresh mutation, not the stale recovered count"
    );
    assert!(store.pending_mutations(10).await.unwrap().is_empty());
    let head: HeadDocument = serde_json::from_slice(
        &backend
            .get(&engine.head_path().unwrap())
            .await
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(head.end_seq, 2);
}

#[tokio::test]
async fn ensure_active_vault_reports_the_recovered_head_advance_not_the_requested_count() {
    let server = TestS3::start("AKID", None).await;
    let backend = s3_backend(&server);
    let (store, _pool) = test_store("device-a").await;
    store
        .queue_local_upsert(snapshot(0, "published-before-crash"), 1_000)
        .await
        .unwrap();
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
    engine.run_once(SyncTrigger::Manual).await.unwrap();

    // 模拟一个滞留的 Publishing 状态：replacement head 声称推进到 end_seq=3，
    // 而请求时的 published_mutation_count=5 与实际推进量（3-1=2）不符。
    let identity = load_versioned_identity(backend.as_ref()).await.unwrap();
    let existing_object = backend.get(&engine.head_path().unwrap()).await.unwrap();
    let existing_head: HeadDocument = serde_json::from_slice(&existing_object.bytes).unwrap();
    let replacement = HeadDocument {
        generation_id: existing_head.generation_id.clone(),
        device_id: existing_head.device_id.clone(),
        end_seq: 3,
        path: existing_head.path.clone(),
        sha256: existing_head.sha256.clone(),
    };
    begin_head_publish(
        backend.as_ref(),
        &identity,
        HeadPublishRequest {
            operation_id: "publish-stuck-advance".into(),
            owner_device_id: "device-a".into(),
            started_at_ms: current_time_millis(),
            lease_expires_at_ms: current_time_millis().saturating_add(DEFAULT_MAINTENANCE_LEASE_MS),
            head_path: engine.head_path().unwrap().display(),
            expected_head_etag: existing_object.etag,
            replacement_head_json: serde_json::to_string(&replacement).unwrap(),
            published_mutation_count: 5,
        },
    )
    .await
    .unwrap();

    let (_vault, recovered) = engine.ensure_active_vault().await.unwrap();

    // 恢复计数必须来自恢复前后 head.end_seq 差值，而非请求时的
    // published_mutation_count。
    assert_eq!(
        recovered,
        Some(("device-a".into(), 2)),
        "recovered count must be the real end_seq advance (3 - 1), not the requested count"
    );
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
        RemotePath::parse("v1/generations/generation-plain/devices/baseline/head.json").unwrap();
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
