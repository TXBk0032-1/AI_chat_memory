use crate::{
    error::{AppError, Result},
    sync::{
        backend::{CloudBackend, RemotePath},
        bundle::{SealedBundle, seal_bundle},
        store::{PendingMutation, SyncStore, current_time_millis},
        types::{BundleChange, BundleContents, SyncTrigger},
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
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
        let published = self.publish_pending().await?;
        Ok(published)
    }

    pub async fn publish_pending(&self) -> Result<SyncReport> {
        let pending = self.store.pending_mutations(500).await?;
        if pending.is_empty() {
            return Ok(SyncReport::default());
        }
        let contents = self.contents_from_pending(&pending)?;
        let sealed = seal_bundle(&contents)?;
        let path = self.bundle_path(&sealed, contents.start_seq, contents.end_seq)?;
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
}
