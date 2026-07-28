use crate::error::{AppError, Result};
use crate::sync::hlc::HybridClock;
use crate::sync::types::{
    EntityKey, EntityVersion, MutationOperation, NormalizedSessionSnapshot, SyncTrigger,
};
use sha2::{Digest, Sha256};
use sqlx::{Sqlite, SqlitePool, Transaction};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceState {
    pub device_id: String,
    pub display_name: String,
    pub hlc_wall_ms: i64,
    pub hlc_counter: i64,
    pub next_seq: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingMutation {
    pub key: EntityKey,
    pub local_seq: i64,
    pub operation: MutationOperation,
    pub version: EntityVersion,
    pub content_hash: Option<String>,
    pub snapshot: Option<NormalizedSessionSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCursor {
    pub generation_id: String,
    pub remote_device_id: String,
    pub cursor_seq: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedBundle {
    pub bundle_sha256: String,
    pub generation_id: String,
    pub stage: String,
    pub staged_at_ms: i64,
    pub published_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncRun {
    pub id: i64,
    pub trigger: String,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub status: String,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SyncStore {
    pool: SqlitePool,
    write_gate: Arc<Mutex<()>>,
}

impl SyncStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            write_gate: Arc::new(Mutex::new(())),
        }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn initialize_device(
        &self,
        device_id: &str,
        display_name: &str,
    ) -> Result<DeviceState> {
        let _gate = self.write_gate.lock().await;
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT OR IGNORE INTO sync_device_state
             (singleton, device_id, display_name, hlc_wall_ms, hlc_counter, next_seq)
             VALUES (1, ?, ?, 0, 0, 1)",
        )
        .bind(device_id)
        .bind(display_name)
        .execute(&mut *tx)
        .await?;
        let state = Self::required_device_state_in(&mut tx).await?;
        tx.commit().await?;
        Ok(state)
    }

    pub async fn device_state(&self) -> Result<Option<DeviceState>> {
        let mut tx = self.pool.begin().await?;
        let state = Self::device_state_in(&mut tx).await?;
        tx.rollback().await?;
        Ok(state)
    }

    async fn device_state_in(tx: &mut Transaction<'_, Sqlite>) -> Result<Option<DeviceState>> {
        Ok(sqlx::query_as::<_, (String, String, i64, i64, i64)>(
            "SELECT device_id, display_name, hlc_wall_ms, hlc_counter, next_seq
             FROM sync_device_state WHERE singleton = 1",
        )
        .fetch_optional(&mut **tx)
        .await?
        .map(
            |(device_id, display_name, hlc_wall_ms, hlc_counter, next_seq)| DeviceState {
                device_id,
                display_name,
                hlc_wall_ms,
                hlc_counter,
                next_seq,
            },
        ))
    }

    async fn required_device_state_in(tx: &mut Transaction<'_, Sqlite>) -> Result<DeviceState> {
        Self::device_state_in(tx)
            .await?
            .ok_or_else(|| AppError::NotFound("sync device state".into()))
    }

    pub async fn queue_local_upsert(
        &self,
        snapshot: NormalizedSessionSnapshot,
        now_ms: i64,
    ) -> Result<PendingMutation> {
        let _gate = self.write_gate.lock().await;
        let mut tx = self.pool.begin().await?;
        let result = self
            .queue_local_upsert_in_unlocked(&mut tx, snapshot, now_ms)
            .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub async fn queue_local_upsert_in(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        snapshot: NormalizedSessionSnapshot,
        now_ms: i64,
    ) -> Result<PendingMutation> {
        self.queue_local_upsert_in_unlocked(tx, snapshot, now_ms)
            .await
    }

    async fn queue_local_upsert_in_unlocked(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        snapshot: NormalizedSessionSnapshot,
        now_ms: i64,
    ) -> Result<PendingMutation> {
        self.queue_local_mutation_in(
            tx,
            MutationOperation::Upsert,
            snapshot.key.clone(),
            Some(snapshot),
            now_ms,
        )
        .await
    }

    pub async fn queue_local_delete(&self, key: EntityKey, now_ms: i64) -> Result<PendingMutation> {
        let _gate = self.write_gate.lock().await;
        let mut tx = self.pool.begin().await?;
        let result = self
            .queue_local_delete_in_unlocked(&mut tx, key, now_ms)
            .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub async fn queue_local_delete_in(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        key: EntityKey,
        now_ms: i64,
    ) -> Result<PendingMutation> {
        self.queue_local_delete_in_unlocked(tx, key, now_ms).await
    }

    async fn queue_local_delete_in_unlocked(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        key: EntityKey,
        now_ms: i64,
    ) -> Result<PendingMutation> {
        self.queue_local_mutation_in(tx, MutationOperation::Delete, key, None, now_ms)
            .await
    }

    async fn queue_local_mutation_in(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        operation: MutationOperation,
        key: EntityKey,
        snapshot: Option<NormalizedSessionSnapshot>,
        now_ms: i64,
    ) -> Result<PendingMutation> {
        sqlx::query("UPDATE sync_device_state SET next_seq = next_seq WHERE singleton = 1")
            .execute(&mut **tx)
            .await?;
        let state = Self::required_device_state_in(tx).await?;
        let mut clock = HybridClock::new(
            state.device_id.clone(),
            state.hlc_wall_ms,
            state.hlc_counter,
        );
        let version = clock
            .tick(now_ms)
            .map_err(|error| AppError::InvalidData(error.to_string()))?;
        let local_seq = state.next_seq;
        let (snapshot_json, content_hash) = match snapshot.as_ref() {
            Some(value) => {
                let json = serde_json::to_string(value)?;
                let hash = hex::encode(Sha256::digest(json.as_bytes()));
                (Some(json), Some(hash))
            }
            None => (None, None),
        };
        let operation_wire = operation_wire(&operation);

        sqlx::query(
            "UPDATE sync_device_state
             SET hlc_wall_ms = ?, hlc_counter = ?, next_seq = next_seq + 1
             WHERE singleton = 1",
        )
        .bind(version.wall_ms)
        .bind(version.counter)
        .execute(&mut **tx)
        .await?;

        sqlx::query(
            "INSERT INTO sync_entity_versions
             (platform, platform_session_id, operation, version_wall_ms, version_counter,
              version_device_id, content_hash)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(platform, platform_session_id) DO UPDATE SET
               operation = excluded.operation,
               version_wall_ms = excluded.version_wall_ms,
               version_counter = excluded.version_counter,
               version_device_id = excluded.version_device_id,
               content_hash = excluded.content_hash
             WHERE (excluded.version_wall_ms, excluded.version_counter, excluded.version_device_id)
                 > (sync_entity_versions.version_wall_ms, sync_entity_versions.version_counter,
                    sync_entity_versions.version_device_id)",
        )
        .bind(&key.platform)
        .bind(&key.platform_session_id)
        .bind(operation_wire)
        .bind(version.wall_ms)
        .bind(version.counter)
        .bind(&version.device_id)
        .bind(&content_hash)
        .execute(&mut **tx)
        .await?;

        sqlx::query(
            "INSERT INTO sync_mutations
             (platform, platform_session_id, local_seq, operation, version_wall_ms,
              version_counter, version_device_id, content_hash, snapshot_json)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(platform, platform_session_id) DO UPDATE SET
               local_seq = excluded.local_seq,
               operation = excluded.operation,
               version_wall_ms = excluded.version_wall_ms,
               version_counter = excluded.version_counter,
               version_device_id = excluded.version_device_id,
               content_hash = excluded.content_hash,
               snapshot_json = excluded.snapshot_json
             WHERE (excluded.version_wall_ms, excluded.version_counter, excluded.version_device_id)
                 > (sync_mutations.version_wall_ms, sync_mutations.version_counter,
                    sync_mutations.version_device_id)",
        )
        .bind(&key.platform)
        .bind(&key.platform_session_id)
        .bind(local_seq)
        .bind(operation_wire)
        .bind(version.wall_ms)
        .bind(version.counter)
        .bind(&version.device_id)
        .bind(&content_hash)
        .bind(&snapshot_json)
        .execute(&mut **tx)
        .await?;

        let row = sqlx::query_as::<
            _,
            (
                i64,
                String,
                i64,
                i64,
                String,
                Option<String>,
                Option<String>,
            ),
        >(
            "SELECT local_seq, operation, version_wall_ms, version_counter, version_device_id,
                    content_hash, snapshot_json
             FROM sync_mutations WHERE platform = ? AND platform_session_id = ?",
        )
        .bind(&key.platform)
        .bind(&key.platform_session_id)
        .fetch_one(&mut **tx)
        .await?;
        mutation_from_row(key, row)
    }

    pub async fn pending_mutations(&self, limit: i64) -> Result<Vec<PendingMutation>> {
        let rows = sqlx::query_as::<
            _,
            (
                String,
                String,
                i64,
                String,
                i64,
                i64,
                String,
                Option<String>,
                Option<String>,
            ),
        >(
            "SELECT platform, platform_session_id, local_seq, operation, version_wall_ms,
                    version_counter, version_device_id, content_hash, snapshot_json
             FROM sync_mutations ORDER BY local_seq LIMIT ?",
        )
        .bind(limit.max(0))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(
                |(
                    platform,
                    platform_session_id,
                    local_seq,
                    operation,
                    wall,
                    counter,
                    device,
                    hash,
                    json,
                )| {
                    mutation_from_row(
                        EntityKey {
                            platform,
                            platform_session_id,
                        },
                        (local_seq, operation, wall, counter, device, hash, json),
                    )
                },
            )
            .collect()
    }

    pub async fn mark_bundle_staged(
        &self,
        bundle_sha256: &str,
        generation_id: &str,
        staged_at_ms: i64,
    ) -> Result<()> {
        let _gate = self.write_gate.lock().await;
        sqlx::query(
            "INSERT INTO sync_published_bundles
             (bundle_sha256, generation_id, stage, staged_at_ms, published_at_ms)
             VALUES (?, ?, 'staged', ?, NULL)
             ON CONFLICT(bundle_sha256) DO UPDATE SET generation_id = CASE
                 WHEN sync_published_bundles.stage = 'published' THEN sync_published_bundles.generation_id
                 ELSE excluded.generation_id END,
               stage = CASE WHEN sync_published_bundles.stage = 'published' THEN 'published' ELSE 'staged' END,
               staged_at_ms = CASE WHEN sync_published_bundles.stage = 'published'
                 THEN sync_published_bundles.staged_at_ms ELSE excluded.staged_at_ms END",
        )
        .bind(bundle_sha256)
        .bind(generation_id)
        .bind(staged_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn mark_bundle_published(
        &self,
        bundle_sha256: &str,
        published_at_ms: i64,
    ) -> Result<()> {
        let _gate = self.write_gate.lock().await;
        sqlx::query(
            "UPDATE sync_published_bundles SET stage = 'published',
               published_at_ms = MAX(COALESCE(published_at_ms, ?), ?)
             WHERE bundle_sha256 = ?",
        )
        .bind(published_at_ms)
        .bind(published_at_ms)
        .bind(bundle_sha256)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn published_bundle(&self, bundle_sha256: &str) -> Result<Option<PublishedBundle>> {
        let bundle = sqlx::query_as::<_, (String, String, String, i64, Option<i64>)>(
            "SELECT bundle_sha256, generation_id, stage, staged_at_ms, published_at_ms
             FROM sync_published_bundles WHERE bundle_sha256 = ?",
        )
        .bind(bundle_sha256)
        .fetch_optional(&self.pool)
        .await?
        .map(
            |(bundle_sha256, generation_id, stage, staged_at_ms, published_at_ms)| {
                PublishedBundle {
                    bundle_sha256,
                    generation_id,
                    stage,
                    staged_at_ms,
                    published_at_ms,
                }
            },
        );
        Ok(bundle)
    }

    pub async fn set_remote_cursor(
        &self,
        generation_id: &str,
        remote_device_id: &str,
        cursor_seq: i64,
        updated_at_ms: i64,
    ) -> Result<()> {
        let _gate = self.write_gate.lock().await;
        sqlx::query(
            "INSERT INTO sync_remote_cursors(generation_id, remote_device_id, cursor_seq, updated_at_ms)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(generation_id, remote_device_id) DO UPDATE SET
               cursor_seq = MAX(sync_remote_cursors.cursor_seq, excluded.cursor_seq),
               updated_at_ms = excluded.updated_at_ms",
        )
        .bind(generation_id)
        .bind(remote_device_id)
        .bind(cursor_seq)
        .bind(updated_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn remote_cursor(
        &self,
        generation_id: &str,
        remote_device_id: &str,
    ) -> Result<Option<RemoteCursor>> {
        let cursor = sqlx::query_as::<_, (String, String, i64, i64)>(
            "SELECT generation_id, remote_device_id, cursor_seq, updated_at_ms
             FROM sync_remote_cursors WHERE generation_id = ? AND remote_device_id = ?",
        )
        .bind(generation_id)
        .bind(remote_device_id)
        .fetch_optional(&self.pool)
        .await?
        .map(
            |(generation_id, remote_device_id, cursor_seq, updated_at_ms)| RemoteCursor {
                generation_id,
                remote_device_id,
                cursor_seq,
                updated_at_ms,
            },
        );
        Ok(cursor)
    }

    pub async fn record_run(
        &self,
        trigger: SyncTrigger,
        started_at_ms: i64,
        finished_at_ms: Option<i64>,
        status: &str,
        error_code: Option<&str>,
    ) -> Result<i64> {
        let _gate = self.write_gate.lock().await;
        let result = sqlx::query(
            "INSERT INTO sync_runs(trigger, started_at_ms, finished_at_ms, status, error_code)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(trigger_wire(trigger))
        .bind(started_at_ms)
        .bind(finished_at_ms)
        .bind(status)
        .bind(error_code)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    pub async fn run(&self, id: i64) -> Result<Option<SyncRun>> {
        let run = sqlx::query_as::<_, (i64, String, i64, Option<i64>, String, Option<String>)>(
            "SELECT id, trigger, started_at_ms, finished_at_ms, status, error_code
             FROM sync_runs WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .map(
            |(id, trigger, started_at_ms, finished_at_ms, status, error_code)| SyncRun {
                id,
                trigger,
                started_at_ms,
                finished_at_ms,
                status,
                error_code,
            },
        );
        Ok(run)
    }
}

fn operation_wire(operation: &MutationOperation) -> &'static str {
    match operation {
        MutationOperation::Upsert => "upsert",
        MutationOperation::Delete => "delete",
    }
}

fn trigger_wire(trigger: SyncTrigger) -> &'static str {
    match trigger {
        SyncTrigger::Startup => "startup",
        SyncTrigger::Periodic => "periodic",
        SyncTrigger::LocalMutation => "local_mutation",
        SyncTrigger::Manual => "manual",
    }
}

fn mutation_from_row(
    key: EntityKey,
    row: (
        i64,
        String,
        i64,
        i64,
        String,
        Option<String>,
        Option<String>,
    ),
) -> Result<PendingMutation> {
    let (local_seq, operation, wall, counter, device, content_hash, snapshot_json) = row;
    let operation = match operation.as_str() {
        "upsert" => MutationOperation::Upsert,
        "delete" => MutationOperation::Delete,
        other => {
            return Err(AppError::InvalidData(format!(
                "unknown mutation operation: {other}"
            )));
        }
    };
    let snapshot = snapshot_json
        .map(|json| serde_json::from_str(&json))
        .transpose()?;
    Ok(PendingMutation {
        key,
        local_seq,
        operation,
        version: EntityVersion::new(wall, counter, device),
        content_hash,
        snapshot,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::connection::{initialize_schema, register_sqlite_vec};
    use crate::sync::types::{EntityKey, NormalizedSessionSnapshot, SyncMessageSnapshot};
    use serde_json::json;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_pool() -> SqlitePool {
        register_sqlite_vec();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        initialize_schema(&pool).await.unwrap();
        pool
    }

    fn snapshot(title: &str) -> NormalizedSessionSnapshot {
        NormalizedSessionSnapshot {
            key: EntityKey {
                platform: "deepseek".into(),
                platform_session_id: "session-1".into(),
            },
            title: title.into(),
            created_at: None,
            updated_at: None,
            imported_at: "2026-01-01T00:00:00Z".into(),
            raw_data: json!({"fixture": true}),
            messages: vec![SyncMessageSnapshot {
                role: "user".into(),
                content: title.into(),
                metadata: json!({}),
                created_at: None,
            }],
        }
    }

    #[tokio::test]
    async fn outbox_coalesces_by_structured_entity_key() {
        let store = SyncStore::new(test_pool().await);
        store.initialize_device("device-a", "Laptop").await.unwrap();
        store
            .queue_local_upsert(snapshot("old"), 1000)
            .await
            .unwrap();
        store
            .queue_local_upsert(snapshot("new"), 1001)
            .await
            .unwrap();
        let pending = store.pending_mutations(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].snapshot.as_ref().unwrap().title, "new");
        assert_eq!(pending[0].version.counter, 0);
    }

    #[tokio::test]
    async fn delete_keeps_tombstone_without_snapshot() {
        let store = SyncStore::new(test_pool().await);
        store.initialize_device("device-a", "Laptop").await.unwrap();
        store
            .queue_local_delete(
                EntityKey {
                    platform: "chat".into(),
                    platform_session_id: "s".into(),
                },
                1000,
            )
            .await
            .unwrap();
        let pending = store.pending_mutations(10).await.unwrap();
        assert_eq!(pending[0].operation, MutationOperation::Delete);
        assert!(pending[0].snapshot.is_none());
        let op: String = sqlx::query_scalar("SELECT operation FROM sync_entity_versions WHERE platform='chat' AND platform_session_id='s'").fetch_one(store.pool()).await.unwrap();
        assert_eq!(op, "delete");
    }

    #[tokio::test]
    async fn initialization_is_idempotent_and_preserves_clock() {
        let store = SyncStore::new(test_pool().await);
        let first = store.initialize_device("device-a", "Laptop").await.unwrap();
        store
            .queue_local_delete(
                EntityKey {
                    platform: "chat".into(),
                    platform_session_id: "s".into(),
                },
                1000,
            )
            .await
            .unwrap();
        let second = store.initialize_device("device-b", "Other").await.unwrap();
        assert_eq!(second.device_id, first.device_id);
        assert_eq!(second.next_seq, 2);
        assert_eq!(second.hlc_wall_ms, 1000);
    }

    #[tokio::test]
    async fn transaction_api_does_not_commit_itself() {
        let pool = test_pool().await;
        let store = SyncStore::new(pool.clone());
        store.initialize_device("device-a", "Laptop").await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        store
            .queue_local_delete_in(
                &mut tx,
                EntityKey {
                    platform: "chat".into(),
                    platform_session_id: "s".into(),
                },
                1000,
            )
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_mutations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
        assert_eq!(store.device_state().await.unwrap().unwrap().next_seq, 1);
    }

    #[tokio::test]
    async fn lower_version_upsert_and_delete_do_not_replace_outbox() {
        let store = SyncStore::new(test_pool().await);
        store.initialize_device("device-a", "Laptop").await.unwrap();
        let newest = snapshot("newest");
        let newest_json = serde_json::to_string(&newest).unwrap();
        sqlx::query(
            "INSERT INTO sync_mutations
             (platform, platform_session_id, local_seq, operation, version_wall_ms,
              version_counter, version_device_id, content_hash, snapshot_json)
             VALUES ('deepseek', 'session-1', 99, 'upsert', 9000, 7, 'device-z', 'newest', ?)",
        )
        .bind(newest_json)
        .execute(store.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sync_entity_versions
             (platform, platform_session_id, operation, version_wall_ms, version_counter,
              version_device_id, content_hash)
             VALUES ('deepseek', 'session-1', 'upsert', 9000, 7, 'device-z', 'newest')",
        )
        .execute(store.pool())
        .await
        .unwrap();

        store
            .queue_local_upsert(snapshot("older"), 1000)
            .await
            .unwrap();
        store
            .queue_local_delete(
                EntityKey {
                    platform: "deepseek".into(),
                    platform_session_id: "session-1".into(),
                },
                1001,
            )
            .await
            .unwrap();

        let pending = store.pending_mutations(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].version, EntityVersion::new(9000, 7, "device-z"));
        assert_eq!(pending[0].operation, MutationOperation::Upsert);
        assert_eq!(pending[0].snapshot.as_ref().unwrap().title, "newest");
        let entity_operation: String = sqlx::query_scalar(
            "SELECT operation FROM sync_entity_versions
             WHERE platform = 'deepseek' AND platform_session_id = 'session-1'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(entity_operation, "upsert");
    }

    #[tokio::test]
    async fn exhausted_hlc_does_not_partially_advance_state_or_outbox() {
        let store = SyncStore::new(test_pool().await);
        store.initialize_device("device-a", "Laptop").await.unwrap();
        sqlx::query(
            "UPDATE sync_device_state SET hlc_wall_ms = ?, hlc_counter = ?, next_seq = 42
             WHERE singleton = 1",
        )
        .bind(i64::MAX)
        .bind(i64::MAX)
        .execute(store.pool())
        .await
        .unwrap();

        let error = store
            .queue_local_upsert(snapshot("blocked"), i64::MAX)
            .await
            .unwrap_err();

        assert!(matches!(error, AppError::InvalidData(_)));
        let state = store.device_state().await.unwrap().unwrap();
        assert_eq!(
            (state.hlc_wall_ms, state.hlc_counter, state.next_seq),
            (i64::MAX, i64::MAX, 42)
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_mutations")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn bundle_cursor_and_run_are_readable() {
        let store = SyncStore::new(test_pool().await);
        store.initialize_device("device-a", "Laptop").await.unwrap();
        store.mark_bundle_staged("abc", "gen-1", 10).await.unwrap();
        store.mark_bundle_published("abc", 20).await.unwrap();
        assert_eq!(
            store.published_bundle("abc").await.unwrap().unwrap().stage,
            "published"
        );
        store
            .set_remote_cursor("gen-1", "device-b", 7, 30)
            .await
            .unwrap();
        assert_eq!(
            store
                .remote_cursor("gen-1", "device-b")
                .await
                .unwrap()
                .unwrap()
                .cursor_seq,
            7
        );
        let id = store
            .record_run(SyncTrigger::Manual, 40, Some(50), "ok", None)
            .await
            .unwrap();
        assert_eq!(store.run(id).await.unwrap().unwrap().status, "ok");
    }

    #[tokio::test]
    async fn concurrent_local_mutations_are_serialized_and_keep_sequences() {
        let path = std::env::temp_dir().join(format!("sync-store-{}.sqlite", uuid::Uuid::new_v4()));
        let store = SyncStore::new(crate::database::connect(&path).await.unwrap());
        store.initialize_device("device-a", "Laptop").await.unwrap();
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let first_store = store.clone();
        let first_barrier = barrier.clone();
        let first = tokio::spawn(async move {
            first_barrier.wait().await;
            first_store
                .queue_local_upsert(snapshot("first"), 1000)
                .await
        });
        let second_store = store.clone();
        let second = tokio::spawn(async move {
            barrier.wait().await;
            second_store
                .queue_local_upsert(
                    NormalizedSessionSnapshot {
                        key: EntityKey {
                            platform: "deepseek".into(),
                            platform_session_id: "session-2".into(),
                        },
                        title: "second".into(),
                        created_at: None,
                        updated_at: None,
                        imported_at: "2026-01-01T00:00:00Z".into(),
                        raw_data: json!({}),
                        messages: vec![],
                    },
                    1000,
                )
                .await
        });
        let (first, second) = tokio::join!(first, second);
        let first = first.unwrap();
        let second = second.unwrap();
        assert!(first.is_ok(), "first mutation failed: {first:?}");
        assert!(second.is_ok(), "second mutation failed: {second:?}");
        let pending = store.pending_mutations(10).await.unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(
            pending
                .iter()
                .map(|item| item.local_seq)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(pending[0].version, EntityVersion::new(1000, 0, "device-a"));
        assert_eq!(pending[1].version, EntityVersion::new(1000, 1, "device-a"));
        store.pool().close().await;
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn published_bundle_is_immutable_across_generations() {
        let store = SyncStore::new(test_pool().await);
        store.initialize_device("device-a", "Laptop").await.unwrap();
        store
            .mark_bundle_staged("same", "generation-a", 10)
            .await
            .unwrap();
        store.mark_bundle_published("same", 20).await.unwrap();
        store
            .mark_bundle_staged("same", "generation-b", 30)
            .await
            .unwrap();
        let bundle = store.published_bundle("same").await.unwrap().unwrap();
        assert_eq!(bundle.generation_id, "generation-a");
        assert_eq!(bundle.stage, "published");
        assert_eq!(bundle.staged_at_ms, 10);
        assert_eq!(bundle.published_at_ms, Some(20));
    }

    #[tokio::test]
    async fn remote_cursor_never_moves_backwards() {
        let store = SyncStore::new(test_pool().await);
        store.initialize_device("device-a", "Laptop").await.unwrap();
        store
            .set_remote_cursor("generation-a", "device-b", 10, 20)
            .await
            .unwrap();
        store
            .set_remote_cursor("generation-a", "device-b", 3, 30)
            .await
            .unwrap();
        let cursor = store
            .remote_cursor("generation-a", "device-b")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cursor.cursor_seq, 10);
        assert_eq!(cursor.updated_at_ms, 30);
    }

    #[tokio::test]
    async fn published_bundle_time_never_moves_backwards() {
        let store = SyncStore::new(test_pool().await);
        store.initialize_device("device-a", "Laptop").await.unwrap();
        store
            .mark_bundle_staged("time", "generation-a", 10)
            .await
            .unwrap();
        store.mark_bundle_published("time", 20).await.unwrap();
        store.mark_bundle_published("time", 15).await.unwrap();
        assert_eq!(
            store
                .published_bundle("time")
                .await
                .unwrap()
                .unwrap()
                .published_at_ms,
            Some(20)
        );
    }

    #[tokio::test]
    async fn long_lived_in_transaction_waits_for_database_write_lock() {
        let path =
            std::env::temp_dir().join(format!("sync-store-long-{}.sqlite", uuid::Uuid::new_v4()));
        let pool = crate::database::connect(&path).await.unwrap();
        let store = SyncStore::new(pool.clone());
        store.initialize_device("device-a", "Laptop").await.unwrap();
        let mut tx_a = pool.begin().await.unwrap();
        store
            .queue_local_delete_in(
                &mut tx_a,
                EntityKey {
                    platform: "chat".into(),
                    platform_session_id: "a".into(),
                },
                1000,
            )
            .await
            .unwrap();
        let store_b = store.clone();
        let pool_b = pool.clone();
        let started = std::sync::Arc::new(tokio::sync::Notify::new());
        let started_b = started.clone();
        let task_b = tokio::spawn(async move {
            let mut tx_b = pool_b.begin().await.unwrap();
            started_b.notify_one();
            let result = store_b
                .queue_local_delete_in(
                    &mut tx_b,
                    EntityKey {
                        platform: "chat".into(),
                        platform_session_id: "b".into(),
                    },
                    1000,
                )
                .await;
            if result.is_ok() {
                tx_b.commit().await.unwrap();
            }
            result
        });
        started.notified().await;
        tokio::task::yield_now().await;
        assert!(
            !task_b.is_finished(),
            "second transaction should wait for the first commit"
        );
        let second_a = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            store.queue_local_delete_in(
                &mut tx_a,
                EntityKey {
                    platform: "chat".into(),
                    platform_session_id: "c".into(),
                },
                1000,
            ),
        )
        .await;
        assert!(
            second_a.is_ok(),
            "transaction A must be able to re-enter _in while B waits"
        );
        second_a.unwrap().unwrap();
        tx_a.commit().await.unwrap();
        task_b.await.unwrap().unwrap();
        let pending = store.pending_mutations(10).await.unwrap();
        assert_eq!(pending.len(), 3);
        pool.close().await;
        let _ = tokio::fs::remove_file(path).await;
    }
}
