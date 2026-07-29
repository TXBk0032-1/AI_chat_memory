use crate::{
    error::{AppError, Result},
    sync::{
        backend::{CloudBackend, RemotePath},
        bundle::{BundleLimits, SealedBundle, open_bundle, seal_bundle},
        merge::MergeEngine,
        store::{PendingMutation, SyncStore, current_time_millis},
        types::{BundleChange, BundleContents, SyncTrigger},
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeadDocument {
    pub generation_id: String,
    pub device_id: String,
    pub end_seq: i64,
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub pulled: usize,
    pub published: usize,
    pub acknowledged: usize,
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

pub struct SyncEngine<B> {
    store: SyncStore,
    backend: Arc<B>,
    vault_id: String,
    generation_id: String,
    device_id: String,
    single_flight: Mutex<()>,
}

impl<B: CloudBackend + 'static> SyncEngine<B> {
    pub fn new(
        store: SyncStore,
        backend: Arc<B>,
        vault_id: impl Into<String>,
        generation_id: impl Into<String>,
        device_id: impl Into<String>,
    ) -> Self {
        Self {
            store,
            backend,
            vault_id: vault_id.into(),
            generation_id: generation_id.into(),
            device_id: device_id.into(),
            single_flight: Mutex::new(()),
        }
    }

    pub async fn run_once(&self, _trigger: SyncTrigger) -> Result<SyncReport> {
        let _guard = self.single_flight.lock().await;
        let mut report = self.pull_remote().await?;
        let published = self.publish_pending().await?;
        report.published += published.published;
        report.acknowledged += published.acknowledged;
        Ok(report)
    }

    async fn pull_remote(&self) -> Result<SyncReport> {
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
        for entry in entries
            .into_iter()
            .filter(|entry| entry.is_collection && entry.name != self.device_id)
        {
            let head_path = devices_path
                .join(&entry.name)
                .and_then(|p| p.join("head.json"))
                .map_err(|e| AppError::InvalidData(e.to_string()))?;
            let head: HeadDocument = serde_json::from_slice(
                &self
                    .backend
                    .get(&head_path)
                    .await
                    .map_err(cloud_error)?
                    .bytes,
            )?;
            let mut chain = Vec::new();
            let mut current = Some(head);
            let cursor = self
                .store
                .remote_cursor(&self.generation_id, &entry.name)
                .await?
                .map(|c| c.cursor_seq)
                .unwrap_or(0);
            while let Some(document) = current {
                if document.end_seq <= cursor {
                    break;
                }
                chain.push(document.clone());
                current = match document.path.is_empty() {
                    true => None,
                    false => match document.path.parse::<String>() {
                        Ok(_) => {
                            let bytes = self
                                .backend
                                .get(
                                    &RemotePath::parse(&document.path)
                                        .map_err(|e| AppError::InvalidData(e.to_string()))?,
                                )
                                .await
                                .map_err(cloud_error)?
                                .bytes;
                            let decoded = open_bundle(&bytes, &BundleLimits::default())?;
                            decoded.header.previous_path.map(|path| HeadDocument {
                                generation_id: decoded.header.generation_id,
                                device_id: decoded.header.device_id,
                                end_seq: decoded.header.previous_end_seq.unwrap_or(0),
                                path,
                                sha256: decoded.header.previous_sha256.unwrap_or_default(),
                            })
                        }
                        Err(_) => None,
                    },
                };
            }
            for document in chain.into_iter().rev() {
                let bytes = self
                    .backend
                    .get(
                        &RemotePath::parse(&document.path)
                            .map_err(|e| AppError::InvalidData(e.to_string()))?,
                    )
                    .await
                    .map_err(cloud_error)?
                    .bytes;
                let decoded = open_bundle(&bytes, &BundleLimits::default())?;
                let expected = self
                    .store
                    .remote_cursor(&self.generation_id, &entry.name)
                    .await?
                    .map(|c| c.cursor_seq)
                    .unwrap_or(cursor);
                let outcome = merger
                    .apply_bundle(&self.generation_id, &entry.name, expected, &decoded)
                    .await?;
                report.pulled += outcome.applied;
            }
        }
        Ok(report)
    }

    /// Rebuilds the local publication boundary under a new generation identifier.
    /// The existing generation remains untouched until the new immutable bundle has
    /// been uploaded and read back successfully.
    pub async fn rewrite_generation(&self, new_generation_id: &str) -> Result<SyncReport> {
        if new_generation_id.is_empty() || new_generation_id == self.generation_id {
            return Err(AppError::InvalidData("new generation id is invalid".into()));
        }
        let _guard = self.single_flight.lock().await;
        let pending = self.store.pending_mutations(500).await?;
        if pending.is_empty() {
            return Ok(SyncReport::default());
        }
        let contents = self.contents_from_pending(&pending)?;
        let sealed = seal_bundle(&contents)?;
        let path = RemotePath::parse(&format!(
            "v1/generations/{new_generation_id}/devices/{}/bundles/{}-{}.acmb",
            self.device_id, contents.end_seq, sealed.file_sha256
        ))
        .map_err(|e| AppError::InvalidData(e.to_string()))?;
        self.ensure_parent_collections(&path).await?;
        self.backend
            .put_immutable(&path, &sealed.bytes)
            .await
            .map_err(cloud_error)?;
        let downloaded = self.backend.get(&path).await.map_err(cloud_error)?;
        if downloaded.bytes != sealed.bytes {
            return Err(AppError::InvalidData(
                "generation rewrite verification failed".into(),
            ));
        }
        Ok(SyncReport {
            published: pending.len(),
            ..SyncReport::default()
        })
    }

    pub async fn publish_pending(&self) -> Result<SyncReport> {
        let pending = self.store.pending_mutations(500).await?;
        if pending.is_empty() {
            return Ok(SyncReport::default());
        }
        let contents = self.contents_from_pending(&pending)?;
        let sealed = seal_bundle(&contents)?;
        let path = self.bundle_path(&sealed, contents.start_seq, contents.end_seq)?;
        self.ensure_parent_collections(&path).await?;
        let digest = sealed.file_sha256.clone();
        self.store
            .mark_bundle_staged(&digest, &self.generation_id, current_time_millis())
            .await?;
        match self.backend.put_immutable(&path, &sealed.bytes).await {
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
        if sha256_hex(&downloaded.bytes) != digest || downloaded.bytes != sealed.bytes {
            return Err(AppError::InvalidData(
                "uploaded bundle verification failed".into(),
            ));
        }
        let head = HeadDocument {
            generation_id: self.generation_id.clone(),
            device_id: self.device_id.clone(),
            end_seq: contents.end_seq,
            path: path.display(),
            sha256: digest.clone(),
        };
        let head_path = self.head_path()?;
        let head_bytes = serde_json::to_vec(&head)?;
        match self.backend.get(&head_path).await {
            Ok(existing) => {
                let etag = existing
                    .etag
                    .ok_or_else(|| AppError::InvalidData("remote head has no ETag".into()))?;
                self.backend
                    .put_if_match(&head_path, &head_bytes, &etag)
                    .await
                    .map_err(cloud_error)?;
            }
            Err(error) if error.kind() == "not_found" => self
                .backend
                .put_if_absent(&head_path, &head_bytes)
                .await
                .map_err(cloud_error)?,
            Err(error) => return Err(cloud_error(error)),
        }
        self.store
            .mark_bundle_published(&digest, current_time_millis())
            .await?;
        let acknowledged = self.store.acknowledge_mutations(&pending).await?;
        Ok(SyncReport {
            pulled: 0,
            published: pending.len(),
            acknowledged,
        })
    }

    fn contents_from_pending(&self, pending: &[PendingMutation]) -> Result<BundleContents> {
        let first = pending
            .first()
            .ok_or_else(|| AppError::InvalidData("cannot bundle empty outbox".into()))?;
        let last = pending.last().unwrap_or(first);
        Ok(BundleContents {
            vault_id: self.vault_id.clone(),
            generation_id: self.generation_id.clone(),
            device_id: self.device_id.clone(),
            start_seq: first.local_seq,
            end_seq: last.local_seq,
            previous_path: None,
            previous_sha256: None,
            previous_end_seq: None,
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

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn cloud_error(error: crate::sync::backend::CloudError) -> AppError {
    let message = match error.kind() {
        "auth" => "WebDAV authentication failed",
        "offline" => "WebDAV endpoint offline",
        "precondition" => "WebDAV precondition failed",
        "not_found" => "WebDAV object not found",
        _ => "WebDAV protocol error",
    };
    AppError::Configuration(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        database::{
            connection::{initialize_schema, register_sqlite_vec},
            import_sessions,
        },
        models::NormalizedSession,
        sync::test_server::TestWebDav,
    };
    use serde_json::json;
    use sqlx::sqlite::SqlitePoolOptions;

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
}
