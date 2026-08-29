use crate::error::{AppError, Result};
use crate::models::{NormalizedMessage, NormalizedSession};
use crate::sync::hlc::HybridClock;
use crate::sync::types::{
    EntityKey, EntityVersion, MutationOperation, NormalizedSessionSnapshot, SyncMessageSnapshot,
    SyncTrigger,
};
use chrono::Utc;
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
pub struct RemoteObjectAnchor {
    pub end_seq: i64,
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCursor {
    pub generation_id: String,
    pub remote_device_id: String,
    pub cursor_seq: i64,
    pub anchor: Option<RemoteObjectAnchor>,
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
pub struct StagedBundle {
    pub bundle_sha256: String,
    pub generation_id: String,
    pub device_id: String,
    pub object_path: String,
    pub start_seq: i64,
    pub end_seq: i64,
    pub bundle_bytes: Vec<u8>,
    pub staged_at_ms: i64,
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

    pub(crate) async fn device_state_in(
        tx: &mut Transaction<'_, Sqlite>,
    ) -> Result<Option<DeviceState>> {
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

    pub(crate) async fn lock_device_state_in(
        tx: &mut Transaction<'_, Sqlite>,
    ) -> Result<Option<DeviceState>> {
        sqlx::query("UPDATE sync_device_state SET next_seq = next_seq WHERE singleton = 1")
            .execute(&mut **tx)
            .await?;
        Self::device_state_in(tx).await
    }

    async fn required_device_state_in(tx: &mut Transaction<'_, Sqlite>) -> Result<DeviceState> {
        Self::lock_device_state_in(tx)
            .await?
            .ok_or_else(|| AppError::NotFound("sync device state".into()))
    }

    pub async fn seed_local_baseline(&self) -> Result<usize> {
        let _gate = self.write_gate.lock().await;
        let mut tx = self.pool.begin().await?;
        Self::required_device_state_in(&mut tx).await?;
        let now_ms = current_time_millis();
        let rows = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
            ),
        >(
            "SELECT id, platform, platform_session_id, title, created_at, updated_at,
                    imported_at, raw_data
             FROM sessions
             WHERE NOT EXISTS (
                 SELECT 1 FROM sync_entity_versions versions
                 WHERE versions.platform = sessions.platform
                   AND versions.platform_session_id = sessions.platform_session_id
             )
               AND NOT EXISTS (
                 SELECT 1 FROM sync_mutations mutations
                 WHERE mutations.platform = sessions.platform
                   AND mutations.platform_session_id = sessions.platform_session_id
             )
             ORDER BY platform, platform_session_id",
        )
        .fetch_all(&mut *tx)
        .await?;
        let mut seeded = 0;
        for (
            id,
            platform,
            platform_session_id,
            title,
            created_at,
            updated_at,
            imported_at,
            raw_data,
        ) in rows
        {
            let messages =
                sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<String>)>(
                    "SELECT role, content, metadata, created_at
                 FROM messages WHERE session_id = ? ORDER BY seq, id",
                )
                .bind(&id)
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .map(|(role, content, metadata, created_at)| {
                    Ok(NormalizedMessage {
                        role,
                        content: content.unwrap_or_default(),
                        metadata: parse_stored_json(metadata)?,
                        created_at,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let session = NormalizedSession {
                id,
                platform,
                platform_session_id,
                title: title.unwrap_or_default(),
                created_at,
                updated_at,
                imported_at: imported_at.unwrap_or_default(),
                messages,
                raw_data: parse_stored_json(raw_data)?,
            };
            self.queue_local_upsert_in(&mut tx, snapshot_from_normalized_session(&session), now_ms)
                .await?;
            seeded += 1;
        }
        tx.commit().await?;
        Ok(seeded)
    }

    pub async fn rebuild_local_baseline(&self) -> Result<usize> {
        let _gate = self.write_gate.lock().await;
        let mut tx = self.pool.begin().await?;
        Self::required_device_state_in(&mut tx).await?;
        let tombstones = sqlx::query_as::<_, (String, String)>(
            "SELECT versions.platform, versions.platform_session_id
             FROM sync_entity_versions versions
             WHERE versions.operation = 'delete'
               AND NOT EXISTS (
                 SELECT 1 FROM sessions
                 WHERE sessions.platform = versions.platform
                   AND sessions.platform_session_id = versions.platform_session_id
               )
             ORDER BY versions.platform, versions.platform_session_id",
        )
        .fetch_all(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM sync_mutations")
            .execute(&mut *tx)
            .await?;
        let rows = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
            ),
        >(
            "SELECT id, platform, platform_session_id, title, created_at, updated_at,
                    imported_at, raw_data
             FROM sessions
             ORDER BY platform, platform_session_id",
        )
        .fetch_all(&mut *tx)
        .await?;
        let now_ms = current_time_millis();
        let mut rebuilt = 0;
        for (
            id,
            platform,
            platform_session_id,
            title,
            created_at,
            updated_at,
            imported_at,
            raw_data,
        ) in rows
        {
            let messages =
                sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<String>)>(
                    "SELECT role, content, metadata, created_at
                     FROM messages WHERE session_id = ? ORDER BY seq, id",
                )
                .bind(&id)
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .map(|(role, content, metadata, created_at)| {
                    Ok(NormalizedMessage {
                        role,
                        content: content.unwrap_or_default(),
                        metadata: parse_stored_json(metadata)?,
                        created_at,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let session = NormalizedSession {
                id,
                platform,
                platform_session_id,
                title: title.unwrap_or_default(),
                created_at,
                updated_at,
                imported_at: imported_at.unwrap_or_default(),
                messages,
                raw_data: parse_stored_json(raw_data)?,
            };
            self.queue_local_upsert_in(&mut tx, snapshot_from_normalized_session(&session), now_ms)
                .await?;
            rebuilt += 1;
        }
        for (platform, platform_session_id) in tombstones {
            self.queue_local_delete_in(
                &mut tx,
                EntityKey {
                    platform,
                    platform_session_id,
                },
                now_ms,
            )
            .await?;
            rebuilt += 1;
        }
        tx.commit().await?;
        Ok(rebuilt)
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

    /// Builds a consistent, read-only generation baseline without changing the live outbox.
    pub async fn baseline_mutations(&self) -> Result<Vec<PendingMutation>> {
        let mut tx = self.pool.begin().await?;
        let mutations = Self::baseline_mutations_in(&mut tx).await?;
        tx.rollback().await?;
        Ok(mutations)
    }

    async fn baseline_mutations_in(
        tx: &mut Transaction<'_, Sqlite>,
    ) -> Result<Vec<PendingMutation>> {
        let rows = sqlx::query_as::<_, (String, String, String, i64, i64, String, Option<String>)>(
            "SELECT platform, platform_session_id, operation, version_wall_ms,
                    version_counter, version_device_id, content_hash
             FROM sync_entity_versions
             ORDER BY CASE operation WHEN 'upsert' THEN 0 ELSE 1 END,
                      platform, platform_session_id",
        )
        .fetch_all(&mut **tx)
        .await?;

        let mut mutations = Vec::with_capacity(rows.len());
        for (
            index,
            (platform, platform_session_id, operation, wall, counter, device, stored_hash),
        ) in rows.into_iter().enumerate()
        {
            let key = EntityKey {
                platform,
                platform_session_id,
            };
            let local_seq = i64::try_from(index)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| AppError::InvalidData("baseline sequence overflow".into()))?;
            let (operation, content_hash, snapshot) = match operation.as_str() {
                "upsert" => {
                    let (id, title, created_at, updated_at, imported_at, raw_data) =
                        sqlx::query_as::<
                            _,
                            (
                                String,
                                Option<String>,
                                Option<String>,
                                Option<String>,
                                Option<String>,
                                Option<String>,
                            ),
                        >(
                            "SELECT id, title, created_at, updated_at, imported_at, raw_data
                             FROM sessions WHERE platform = ? AND platform_session_id = ?",
                        )
                        .bind(&key.platform)
                        .bind(&key.platform_session_id)
                        .fetch_optional(&mut **tx)
                        .await?
                        .ok_or_else(|| {
                            AppError::InvalidData(
                                "upsert entity version has no local session snapshot".into(),
                            )
                        })?;
                    let messages = sqlx::query_as::<
                        _,
                        (String, Option<String>, Option<String>, Option<String>),
                    >(
                        "SELECT role, content, metadata, created_at
                         FROM messages WHERE session_id = ? ORDER BY seq, id",
                    )
                    .bind(id)
                    .fetch_all(&mut **tx)
                    .await?
                    .into_iter()
                    .map(|(role, content, metadata, created_at)| {
                        Ok(SyncMessageSnapshot {
                            role,
                            content: content.unwrap_or_default(),
                            metadata: parse_stored_json(metadata)?,
                            created_at,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                    let snapshot = NormalizedSessionSnapshot {
                        key: key.clone(),
                        title: title.unwrap_or_default(),
                        created_at,
                        updated_at,
                        imported_at: imported_at.unwrap_or_default(),
                        raw_data: parse_stored_json(raw_data)?,
                        messages,
                    };
                    let computed_hash = hex::encode(Sha256::digest(serde_json::to_vec(&snapshot)?));
                    if stored_hash.as_deref() != Some(computed_hash.as_str()) {
                        return Err(AppError::InvalidData(
                            "local session snapshot does not match its entity version hash".into(),
                        ));
                    }
                    (
                        MutationOperation::Upsert,
                        Some(computed_hash),
                        Some(snapshot),
                    )
                }
                "delete" => (MutationOperation::Delete, None, None),
                other => {
                    return Err(AppError::InvalidData(format!(
                        "unknown entity version operation: {other}"
                    )));
                }
            };
            mutations.push(PendingMutation {
                key,
                local_seq,
                operation,
                version: EntityVersion::new(wall, counter, device),
                content_hash,
                snapshot,
            });
        }
        Ok(mutations)
    }

    /// Re-enqueues the complete winning local state for a new remote publication boundary.
    ///
    /// Entity versions and tombstones are preserved exactly. The outbox replacement and
    /// publication marker are committed atomically so a crash cannot mark a generation as
    /// replayed without also retaining every mutation needed to publish it.
    pub async fn replay_local_baseline_for_generation(
        &self,
        vault_id: &str,
        generation_id: &str,
    ) -> Result<usize> {
        if vault_id.is_empty() || generation_id.is_empty() {
            return Err(AppError::InvalidData(
                "publication vault and generation must not be empty".into(),
            ));
        }
        let _gate = self.write_gate.lock().await;
        let mut tx = self.pool.begin().await?;
        let current: Option<(String, String)> = sqlx::query_as(
            "SELECT vault_id, generation_id FROM sync_publication_state WHERE singleton = 1",
        )
        .fetch_optional(&mut *tx)
        .await?;
        if current
            .as_ref()
            .is_some_and(|(vault, generation)| vault == vault_id && generation == generation_id)
        {
            tx.rollback().await?;
            return Ok(0);
        }

        let state = Self::required_device_state_in(&mut tx).await?;
        let baseline = Self::baseline_mutations_in(&mut tx).await?;
        let replayed = baseline.len();
        let replayed_i64 = i64::try_from(replayed)
            .map_err(|_| AppError::InvalidData("publication baseline is too large".into()))?;
        let next_seq = state
            .next_seq
            .checked_add(replayed_i64)
            .ok_or_else(|| AppError::InvalidData("publication sequence overflow".into()))?;

        sqlx::query("DELETE FROM sync_mutations")
            .execute(&mut *tx)
            .await?;
        for (index, mutation) in baseline.into_iter().enumerate() {
            let index = i64::try_from(index)
                .map_err(|_| AppError::InvalidData("publication sequence overflow".into()))?;
            let local_seq = state
                .next_seq
                .checked_add(index)
                .ok_or_else(|| AppError::InvalidData("publication sequence overflow".into()))?;
            let snapshot_json = mutation
                .snapshot
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?;
            sqlx::query(
                "INSERT INTO sync_mutations
                 (platform, platform_session_id, local_seq, operation, version_wall_ms,
                  version_counter, version_device_id, content_hash, snapshot_json)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&mutation.key.platform)
            .bind(&mutation.key.platform_session_id)
            .bind(local_seq)
            .bind(operation_wire(&mutation.operation))
            .bind(mutation.version.wall_ms)
            .bind(mutation.version.counter)
            .bind(&mutation.version.device_id)
            .bind(&mutation.content_hash)
            .bind(snapshot_json)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("UPDATE sync_device_state SET next_seq = ? WHERE singleton = 1")
            .bind(next_seq)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO sync_publication_state(singleton, vault_id, generation_id)
             VALUES (1, ?, ?)
             ON CONFLICT(singleton) DO UPDATE SET
               vault_id = excluded.vault_id,
               generation_id = excluded.generation_id",
        )
        .bind(vault_id)
        .bind(generation_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(replayed)
    }
    pub async fn restore_mutations(&self, mutations: &[PendingMutation]) -> Result<usize> {
        let _gate = self.write_gate.lock().await;
        let mut tx = self.pool.begin().await?;
        let mut restored = 0;
        for mutation in mutations {
            let snapshot_json = mutation
                .snapshot
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?;
            let result = sqlx::query(
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
            .bind(&mutation.key.platform)
            .bind(&mutation.key.platform_session_id)
            .bind(mutation.local_seq)
            .bind(operation_wire(&mutation.operation))
            .bind(mutation.version.wall_ms)
            .bind(mutation.version.counter)
            .bind(&mutation.version.device_id)
            .bind(&mutation.content_hash)
            .bind(snapshot_json)
            .execute(&mut *tx)
            .await?;
            restored += result.rows_affected() as usize;
        }
        tx.commit().await?;
        Ok(restored)
    }
    pub async fn acknowledge_mutations(&self, mutations: &[PendingMutation]) -> Result<usize> {
        let _gate = self.write_gate.lock().await;
        let mut tx = self.pool.begin().await?;
        let mut removed = 0;
        for mutation in mutations {
            let result = sqlx::query(
                "DELETE FROM sync_mutations
                 WHERE platform = ? AND platform_session_id = ? AND local_seq = ?
                   AND version_wall_ms = ? AND version_counter = ? AND version_device_id = ?",
            )
            .bind(&mutation.key.platform)
            .bind(&mutation.key.platform_session_id)
            .bind(mutation.local_seq)
            .bind(mutation.version.wall_ms)
            .bind(mutation.version.counter)
            .bind(&mutation.version.device_id)
            .execute(&mut *tx)
            .await?;
            removed += result.rows_affected() as usize;
        }
        tx.commit().await?;
        Ok(removed)
    }

    /// Persists a fully sealed, content-addressed bundle before any remote write.
    ///
    /// The blob may be ciphertext. It never contains the sync passphrase or cloud credentials.
    #[allow(clippy::too_many_arguments)]
    pub async fn stage_bundle(
        &self,
        bundle_sha256: &str,
        generation_id: &str,
        device_id: &str,
        object_path: &str,
        start_seq: i64,
        end_seq: i64,
        bundle_bytes: &[u8],
        staged_at_ms: i64,
    ) -> Result<()> {
        if bundle_sha256.is_empty()
            || generation_id.is_empty()
            || device_id.is_empty()
            || object_path.is_empty()
            || start_seq < 1
            || end_seq < start_seq
            || bundle_bytes.is_empty()
        {
            return Err(AppError::InvalidData(
                "staged bundle recovery record is incomplete".into(),
            ));
        }
        let _gate = self.write_gate.lock().await;
        sqlx::query(
            "INSERT INTO sync_published_bundles
             (bundle_sha256, generation_id, device_id, object_path, start_seq, end_seq,
              bundle_bytes, stage, staged_at_ms, published_at_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, 'staged', ?, NULL)",
        )
        .bind(bundle_sha256)
        .bind(generation_id)
        .bind(device_id)
        .bind(object_path)
        .bind(start_seq)
        .bind(end_seq)
        .bind(bundle_bytes)
        .bind(staged_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
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

    pub async fn staged_bundle(
        &self,
        generation_id: &str,
        device_id: &str,
        start_seq: i64,
    ) -> Result<Option<StagedBundle>> {
        let row = sqlx::query_as::<_, (String, String, String, String, i64, i64, Vec<u8>, i64)>(
            "SELECT bundle_sha256, generation_id, device_id, object_path, start_seq, end_seq,
                    bundle_bytes, staged_at_ms
             FROM sync_published_bundles
             WHERE generation_id = ? AND device_id = ? AND stage = 'staged'
               AND start_seq = ? AND bundle_bytes IS NOT NULL
             ORDER BY staged_at_ms, bundle_sha256 LIMIT 1",
        )
        .bind(generation_id)
        .bind(device_id)
        .bind(start_seq)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(
            |(
                bundle_sha256,
                generation_id,
                device_id,
                object_path,
                start_seq,
                end_seq,
                bundle_bytes,
                staged_at_ms,
            )| StagedBundle {
                bundle_sha256,
                generation_id,
                device_id,
                object_path,
                start_seq,
                end_seq,
                bundle_bytes,
                staged_at_ms,
            },
        ))
    }

    pub async fn remove_staged_bundle(&self, bundle_sha256: &str) -> Result<()> {
        let _gate = self.write_gate.lock().await;
        sqlx::query(
            "DELETE FROM sync_published_bundles WHERE bundle_sha256 = ? AND stage = 'staged'",
        )
        .bind(bundle_sha256)
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
               published_at_ms = MAX(COALESCE(published_at_ms, ?), ?), bundle_bytes = NULL
             WHERE bundle_sha256 = ?",
        )
        .bind(published_at_ms)
        .bind(published_at_ms)
        .bind(bundle_sha256)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Removes `stage='published'` rows older than `older_than_ms` so the
    /// `sync_published_bundles` table does not grow without bound. Recent
    /// rows are retained because `published_bundle` is consulted for publish
    /// idempotency within the current generation, and a short retention window
    /// protects against re-publishing a just-acknowledged bundle after a restart.
    pub async fn prune_published_bundles(&self, older_than_ms: i64) -> Result<u64> {
        let _gate = self.write_gate.lock().await;
        let result = sqlx::query(
            "DELETE FROM sync_published_bundles
             WHERE stage = 'published' AND published_at_ms IS NOT NULL
               AND published_at_ms < ?",
        )
        .bind(older_than_ms)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
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

    pub async fn pending_mutation_count(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM sync_mutations")
            .fetch_one(&self.pool)
            .await?)
    }

    pub async fn remove_remote_cursor(
        &self,
        generation_id: &str,
        remote_device_id: &str,
    ) -> Result<()> {
        let _gate = self.write_gate.lock().await;
        sqlx::query(
            "DELETE FROM sync_remote_cursors
             WHERE generation_id = ? AND remote_device_id = ?",
        )
        .bind(generation_id)
        .bind(remote_device_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
    pub async fn set_remote_cursor(
        &self,
        generation_id: &str,
        remote_device_id: &str,
        anchor: &RemoteObjectAnchor,
        updated_at_ms: i64,
    ) -> Result<()> {
        validate_remote_anchor(anchor)?;
        let _gate = self.write_gate.lock().await;
        sqlx::query(
            "INSERT INTO sync_remote_cursors(
               generation_id, remote_device_id, cursor_seq,
               anchor_end_seq, anchor_path, anchor_sha256, updated_at_ms
             )
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(generation_id, remote_device_id) DO UPDATE SET
               cursor_seq = MAX(sync_remote_cursors.cursor_seq, excluded.cursor_seq),
               anchor_end_seq = CASE
                 WHEN excluded.cursor_seq >= sync_remote_cursors.cursor_seq THEN excluded.anchor_end_seq
                 ELSE sync_remote_cursors.anchor_end_seq
               END,
               anchor_path = CASE
                 WHEN excluded.cursor_seq >= sync_remote_cursors.cursor_seq THEN excluded.anchor_path
                 ELSE sync_remote_cursors.anchor_path
               END,
               anchor_sha256 = CASE
                 WHEN excluded.cursor_seq >= sync_remote_cursors.cursor_seq THEN excluded.anchor_sha256
                 ELSE sync_remote_cursors.anchor_sha256
               END,
               updated_at_ms = excluded.updated_at_ms",
        )
        .bind(generation_id)
        .bind(remote_device_id)
        .bind(anchor.end_seq)
        .bind(anchor.end_seq)
        .bind(&anchor.path)
        .bind(&anchor.sha256)
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
        let cursor = sqlx::query_as::<
            _,
            (
                String,
                String,
                i64,
                Option<i64>,
                Option<String>,
                Option<String>,
                i64,
            ),
        >(
            "SELECT generation_id, remote_device_id, cursor_seq,
                    anchor_end_seq, anchor_path, anchor_sha256, updated_at_ms
             FROM sync_remote_cursors WHERE generation_id = ? AND remote_device_id = ?",
        )
        .bind(generation_id)
        .bind(remote_device_id)
        .fetch_optional(&self.pool)
        .await?
        .map(
            |(
                generation_id,
                remote_device_id,
                cursor_seq,
                anchor_end_seq,
                anchor_path,
                anchor_sha256,
                updated_at_ms,
            )| {
                let anchor = match (anchor_end_seq, anchor_path, anchor_sha256) {
                    (Some(end_seq), Some(path), Some(sha256)) => Some(RemoteObjectAnchor {
                        end_seq,
                        path,
                        sha256,
                    }),
                    (None, None, None) => None,
                    _ => {
                        return Err(AppError::InvalidData(
                            "remote cursor anchor is incomplete".into(),
                        ));
                    }
                };
                if cursor_seq == 0 && anchor.is_none() {
                    return Ok(RemoteCursor {
                        generation_id,
                        remote_device_id,
                        cursor_seq,
                        anchor,
                        updated_at_ms,
                    });
                }
                let anchor = anchor.ok_or_else(|| {
                    AppError::InvalidData("remote cursor is missing its object anchor".into())
                })?;
                validate_remote_anchor(&anchor)?;
                if anchor.end_seq != cursor_seq {
                    return Err(AppError::InvalidData(
                        "remote cursor does not match its object anchor".into(),
                    ));
                }
                Ok(RemoteCursor {
                    generation_id,
                    remote_device_id,
                    cursor_seq,
                    anchor: Some(anchor),
                    updated_at_ms,
                })
            },
        )
        .transpose()?;
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

pub(crate) fn current_time_millis() -> i64 {
    Utc::now().timestamp_millis()
}

pub(crate) fn snapshot_from_normalized_session(
    session: &NormalizedSession,
) -> NormalizedSessionSnapshot {
    NormalizedSessionSnapshot {
        key: EntityKey {
            platform: session.platform.clone(),
            platform_session_id: session.platform_session_id.clone(),
        },
        title: session.title.clone(),
        created_at: session.created_at.clone(),
        updated_at: session.updated_at.clone(),
        imported_at: session.imported_at.clone(),
        raw_data: session.raw_data.clone(),
        messages: session
            .messages
            .iter()
            .map(|message| SyncMessageSnapshot {
                role: message.role.clone(),
                content: message.content.clone(),
                metadata: message.metadata.clone(),
                created_at: message.created_at.clone(),
            })
            .collect(),
    }
}

fn validate_remote_anchor(anchor: &RemoteObjectAnchor) -> Result<()> {
    if anchor.end_seq < 1
        || anchor.path.is_empty()
        || !anchor.path.ends_with(".acmb")
        || anchor.sha256.len() != 64
        || !anchor.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AppError::InvalidData(
            "remote cursor object anchor is invalid".into(),
        ));
    }
    Ok(())
}

fn parse_stored_json(value: Option<String>) -> Result<serde_json::Value> {
    value
        .map(|value| serde_json::from_str(&value))
        .transpose()
        .map(|value| value.unwrap_or(serde_json::Value::Null))
        .map_err(Into::into)
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
    use crate::normalizer::normalize_session;
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

    fn anchor(end_seq: i64) -> RemoteObjectAnchor {
        RemoteObjectAnchor {
            end_seq,
            path: format!(
                "v1/generations/gen-1/devices/device-b/bundles/{end_seq}-{end_seq}-{}.acmb",
                "ab".repeat(32)
            ),
            sha256: "ab".repeat(32),
        }
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
            .set_remote_cursor("gen-1", "device-b", &anchor(7), 30)
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
            .set_remote_cursor("generation-a", "device-b", &anchor(10), 20)
            .await
            .unwrap();
        store
            .set_remote_cursor("generation-a", "device-b", &anchor(3), 30)
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

    #[tokio::test]
    async fn local_mutation_rechecks_device_after_initialization_gap() {
        let pool = test_pool().await;
        let local_store = SyncStore::new(pool.clone());
        let enabling_store = SyncStore::new(pool.clone());
        let observed_without_device = std::sync::Arc::new(tokio::sync::Notify::new());
        let resume_mutation = std::sync::Arc::new(tokio::sync::Notify::new());
        let mutation_observed = observed_without_device.clone();
        let mutation_resume = resume_mutation.clone();
        let mutation_pool = pool.clone();
        let session = normalize_session(
            "custom",
            &json!({
                "id": "during-enable",
                "title": "During enable",
                "messages": [{"role": "user", "content": "captured"}]
            }),
        )
        .unwrap();
        let mutation_task = tokio::spawn(async move {
            assert!(
                SyncStore::new(mutation_pool.clone())
                    .device_state()
                    .await?
                    .is_none()
            );
            mutation_observed.notify_one();
            mutation_resume.notified().await;
            crate::service::import_local_sessions(&mutation_pool, &[session]).await
        });

        observed_without_device.notified().await;
        enabling_store
            .initialize_device("device-a", "Laptop")
            .await
            .unwrap();
        assert_eq!(enabling_store.seed_local_baseline().await.unwrap(), 0);
        resume_mutation.notify_one();

        assert_eq!(mutation_task.await.unwrap().unwrap(), 1);
        let pending = local_store.pending_mutations(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].key.platform_session_id, "during-enable");
        assert_eq!(pending[0].operation, MutationOperation::Upsert);
    }

    #[tokio::test]
    async fn seed_local_baseline_creates_each_existing_upsert_only_once() {
        let pool = test_pool().await;
        let sessions = [
            normalize_session(
                "custom",
                &json!({
                    "id": "baseline-a",
                    "title": "Baseline A",
                    "messages": [
                        {"role": "user", "content": "a-0", "metadata": {"seq": 0}},
                        {"role": "assistant", "content": "a-1", "metadata": {"seq": 1}}
                    ]
                }),
            )
            .unwrap(),
            normalize_session(
                "custom",
                &json!({
                    "id": "baseline-b",
                    "title": "Baseline B",
                    "messages": [{"role": "user", "content": "b-0"}]
                }),
            )
            .unwrap(),
        ];
        crate::database::import_sessions(&pool, &sessions, false)
            .await
            .unwrap();
        let store = SyncStore::new(pool.clone());
        store.initialize_device("device-a", "Laptop").await.unwrap();

        assert_eq!(store.seed_local_baseline().await.unwrap(), 2);
        let first_pending = store.pending_mutations(10).await.unwrap();
        assert_eq!(first_pending.len(), 2);
        let version_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_entity_versions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(version_count, 2);
        assert!(
            first_pending
                .iter()
                .all(|mutation| mutation.operation == MutationOperation::Upsert)
        );
        assert_eq!(
            first_pending[0].snapshot.as_ref().unwrap().messages.len(),
            2
        );
        assert_eq!(
            first_pending[0].snapshot.as_ref().unwrap().messages[0].content,
            "a-0"
        );
        assert_eq!(
            first_pending[0].snapshot.as_ref().unwrap().messages[1].content,
            "a-1"
        );
        for (mutation, session) in first_pending.iter().zip(&sessions) {
            let expected = NormalizedSessionSnapshot {
                key: EntityKey {
                    platform: session.platform.clone(),
                    platform_session_id: session.platform_session_id.clone(),
                },
                title: session.title.clone(),
                created_at: session.created_at.clone(),
                updated_at: session.updated_at.clone(),
                imported_at: session.imported_at.clone(),
                raw_data: session.raw_data.clone(),
                messages: session
                    .messages
                    .iter()
                    .map(|message| SyncMessageSnapshot {
                        role: message.role.clone(),
                        content: message.content.clone(),
                        metadata: message.metadata.clone(),
                        created_at: message.created_at.clone(),
                    })
                    .collect(),
            };
            assert_eq!(mutation.snapshot.as_ref(), Some(&expected));
            let wire = serde_json::to_value(mutation.snapshot.as_ref().unwrap()).unwrap();
            assert!(wire.get("id").is_none());
            assert!(wire["messages"].as_array().unwrap().iter().all(|message| {
                message.get("id").is_none() && message.get("session_id").is_none()
            }));
        }
        assert_eq!(store.seed_local_baseline().await.unwrap(), 0);
        assert_eq!(store.pending_mutations(10).await.unwrap(), first_pending);
        assert_eq!(store.device_state().await.unwrap().unwrap().next_seq, 3);
    }

    #[tokio::test]
    async fn seed_local_baseline_rolls_back_all_rows_when_later_upsert_fails() {
        let pool = test_pool().await;
        let sessions = [
            snapshot("first"),
            NormalizedSessionSnapshot {
                key: EntityKey {
                    platform: "deepseek".into(),
                    platform_session_id: "session-2".into(),
                },
                title: "second".into(),
                created_at: None,
                updated_at: None,
                imported_at: "2026-01-01T00:00:00Z".into(),
                raw_data: json!({"fixture": 2}),
                messages: vec![],
            },
        ];
        for (index, session) in sessions.iter().enumerate() {
            sqlx::query(
                "INSERT INTO sessions
                 (id, platform, platform_session_id, title, created_at, updated_at, imported_at, raw_data)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(format!("local-{index}"))
            .bind(&session.key.platform)
            .bind(&session.key.platform_session_id)
            .bind(&session.title)
            .bind(&session.created_at)
            .bind(&session.updated_at)
            .bind(&session.imported_at)
            .bind(session.raw_data.to_string())
            .execute(&pool)
            .await
            .unwrap();
        }
        let store = SyncStore::new(pool.clone());
        store.initialize_device("device-a", "Laptop").await.unwrap();
        sqlx::query("UPDATE sync_device_state SET hlc_wall_ms = ?, hlc_counter = ?, next_seq = 5")
            .bind(i64::MAX)
            .bind(i64::MAX - 1)
            .execute(&pool)
            .await
            .unwrap();

        let error = store.seed_local_baseline().await.unwrap_err();

        assert!(matches!(error, AppError::InvalidData(_)));
        let mutation_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_mutations")
            .fetch_one(&pool)
            .await
            .unwrap();
        let version_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_entity_versions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((mutation_count, version_count), (0, 0));
        let state = store.device_state().await.unwrap().unwrap();
        assert_eq!(
            (state.hlc_wall_ms, state.hlc_counter, state.next_seq),
            (i64::MAX, i64::MAX - 1, 5)
        );
    }

    #[tokio::test]
    async fn rebuild_local_baseline_includes_all_sessions_and_delete_tombstones() {
        let pool = test_pool().await;
        let store = SyncStore::new(pool.clone());
        store.initialize_device("device-a", "Laptop").await.unwrap();
        let session = normalize_session(
            "custom",
            &json!({
                "id": "live-session",
                "title": "Live",
                "messages": [{"role": "user", "content": "kept"}]
            }),
        )
        .unwrap();
        crate::database::import_sessions(&pool, &[session], false)
            .await
            .unwrap();
        store
            .queue_local_delete(
                EntityKey {
                    platform: "custom".into(),
                    platform_session_id: "deleted-session".into(),
                },
                1_000,
            )
            .await
            .unwrap();
        sqlx::query("DELETE FROM sync_mutations")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(store.rebuild_local_baseline().await.unwrap(), 2);

        let pending = store.pending_mutations(10).await.unwrap();
        assert_eq!(pending.len(), 2);
        assert!(pending.iter().any(|mutation| {
            mutation.key.platform_session_id == "live-session"
                && mutation.operation == MutationOperation::Upsert
                && mutation.snapshot.is_some()
        }));
        assert!(pending.iter().any(|mutation| {
            mutation.key.platform_session_id == "deleted-session"
                && mutation.operation == MutationOperation::Delete
                && mutation.snapshot.is_none()
        }));
    }

    #[tokio::test]
    async fn generation_replay_preserves_versions_and_tombstones_and_is_idempotent() {
        let pool = test_pool().await;
        let store = SyncStore::new(pool.clone());
        store.initialize_device("device-a", "Laptop").await.unwrap();
        let session = normalize_session(
            "custom",
            &json!({
                "id": "live-session",
                "title": "Live",
                "messages": [{"role": "user", "content": "kept"}]
            }),
        )
        .unwrap();
        crate::database::import_sessions(&pool, &[session], false)
            .await
            .unwrap();
        store.seed_local_baseline().await.unwrap();
        store
            .queue_local_delete(
                EntityKey {
                    platform: "custom".into(),
                    platform_session_id: "deleted-session".into(),
                },
                2_000,
            )
            .await
            .unwrap();
        let versions_before = store
            .baseline_mutations()
            .await
            .unwrap()
            .into_iter()
            .map(|mutation| {
                (
                    mutation.key.clone(),
                    (
                        mutation.operation,
                        mutation.version,
                        mutation.content_hash,
                        mutation.snapshot,
                    ),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        let state_before = store.device_state().await.unwrap().unwrap();
        let next_seq_before = state_before.next_seq;

        assert_eq!(
            store
                .replay_local_baseline_for_generation("vault-a", "generation-new")
                .await
                .unwrap(),
            2
        );

        let pending = store.pending_mutations(10).await.unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(
            pending
                .iter()
                .map(|mutation| mutation.local_seq)
                .collect::<Vec<_>>(),
            vec![next_seq_before, next_seq_before + 1]
        );
        for mutation in &pending {
            let expected = versions_before.get(&mutation.key).unwrap();
            assert_eq!(
                (
                    &mutation.operation,
                    &mutation.version,
                    &mutation.content_hash,
                    &mutation.snapshot,
                ),
                (&expected.0, &expected.1, &expected.2, &expected.3)
            );
        }
        assert!(pending.iter().any(|mutation| {
            mutation.key.platform_session_id == "deleted-session"
                && mutation.operation == MutationOperation::Delete
                && mutation.snapshot.is_none()
        }));
        let marker: (String, String) = sqlx::query_as(
            "SELECT vault_id, generation_id FROM sync_publication_state WHERE singleton = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(marker, ("vault-a".into(), "generation-new".into()));
        let state_after = store.device_state().await.unwrap().unwrap();
        assert_eq!(state_after.hlc_wall_ms, state_before.hlc_wall_ms);
        assert_eq!(state_after.hlc_counter, state_before.hlc_counter);
        assert_eq!(state_after.next_seq, next_seq_before + 2);

        assert_eq!(
            store
                .replay_local_baseline_for_generation("vault-a", "generation-new")
                .await
                .unwrap(),
            0
        );
        assert_eq!(store.pending_mutations(10).await.unwrap(), pending);
        assert_eq!(store.device_state().await.unwrap().unwrap(), state_after);
    }

    #[tokio::test]
    async fn generation_replay_rolls_back_outbox_marker_and_sequence_together() {
        let pool = test_pool().await;
        let store = SyncStore::new(pool.clone());
        store.initialize_device("device-a", "Laptop").await.unwrap();
        store
            .queue_local_delete(
                EntityKey {
                    platform: "custom".into(),
                    platform_session_id: "deleted-session".into(),
                },
                1_000,
            )
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO sync_publication_state(singleton, vault_id, generation_id)
             VALUES (1, 'vault-old', 'generation-old')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let pending_before = store.pending_mutations(10).await.unwrap();
        let device_before = store.device_state().await.unwrap().unwrap();
        sqlx::query(
            "CREATE TRIGGER reject_publication_marker_update
             BEFORE UPDATE ON sync_publication_state
             BEGIN SELECT RAISE(ABORT, 'marker write failed'); END",
        )
        .execute(&pool)
        .await
        .unwrap();

        assert!(
            store
                .replay_local_baseline_for_generation("vault-new", "generation-new")
                .await
                .is_err()
        );

        assert_eq!(store.pending_mutations(10).await.unwrap(), pending_before);
        assert_eq!(store.device_state().await.unwrap().unwrap(), device_before);
        let marker: (String, String) = sqlx::query_as(
            "SELECT vault_id, generation_id FROM sync_publication_state WHERE singleton = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(marker, ("vault-old".into(), "generation-old".into()));
    }

    #[tokio::test]
    async fn prune_published_bundles_removes_old_rows_and_keeps_recent() {
        let store = SyncStore::new(test_pool().await);
        store.initialize_device("device-a", "Laptop").await.unwrap();
        // A "stale" published row (published in the distant past) and a "recent"
        // published row. prune_published_bundles(cutoff) should delete only the
        // stale one; staged rows must never be touched.
        let stale_ms: i64 = 1_000_000;
        let recent_ms: i64 = 2_000_000_000;
        store
            .mark_bundle_staged("stale-sha", "generation", stale_ms)
            .await
            .unwrap();
        store
            .mark_bundle_published("stale-sha", stale_ms)
            .await
            .unwrap();
        store
            .mark_bundle_staged("recent-sha", "generation", recent_ms)
            .await
            .unwrap();
        store
            .mark_bundle_published("recent-sha", recent_ms)
            .await
            .unwrap();
        // A still-staged (not yet published) row must be preserved regardless of age.
        store
            .stage_bundle(
                "staged-sha",
                "generation",
                "device-a",
                "v1/path",
                1,
                2,
                &[0u8; 4],
                stale_ms,
            )
            .await
            .unwrap();

        let removed = store.prune_published_bundles(recent_ms).await.unwrap();
        assert_eq!(removed, 1);

        let remaining: Vec<(String, String)> = sqlx::query_as(
            "SELECT bundle_sha256, stage FROM sync_published_bundles ORDER BY bundle_sha256",
        )
        .fetch_all(store.pool())
        .await
        .unwrap();
        let map: std::collections::HashMap<String, String> = remaining.into_iter().collect();
        // Stale published row was pruned; recent published + staged rows survive.
        assert!(!map.contains_key("stale-sha"));
        assert_eq!(map.get("recent-sha").map(String::as_str), Some("published"));
        assert_eq!(map.get("staged-sha").map(String::as_str), Some("staged"));
    }
}
