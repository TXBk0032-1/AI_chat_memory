#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        database::connection::{initialize_schema, register_sqlite_vec},
        sync::{
            bundle::{BundleHeader, CompressionAlgorithm, DecodedBundle, ProtectionAlgorithm},
            store::{RemoteObjectAnchor, SyncStore},
            types::{
                BundleChange, BundleContents, EntityKey, EntityVersion, MutationOperation,
                NormalizedSessionSnapshot,
            },
        },
    };
    use serde_json::json;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> sqlx::SqlitePool {
        register_sqlite_vec();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        initialize_schema(&pool).await.unwrap();
        SyncStore::new(pool.clone())
            .initialize_device("local", "Local")
            .await
            .unwrap();
        pool
    }

    fn bundle(seq: i64, version: i64, operation: MutationOperation) -> DecodedBundle {
        let key = EntityKey {
            platform: "chat".into(),
            platform_session_id: "session-1".into(),
        };
        let snapshot =
            (operation == MutationOperation::Upsert).then(|| NormalizedSessionSnapshot {
                key: key.clone(),
                title: format!("title-{version}"),
                created_at: None,
                updated_at: None,
                imported_at: "2026-07-29T00:00:00Z".into(),
                raw_data: json!({"version": version}),
                messages: vec![],
            });
        let content_hash = snapshot.as_ref().map(|snapshot| {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(serde_json::to_vec(snapshot).unwrap()))
        });
        let previous_end_seq = (seq > 1).then_some(seq - 1);
        let contents = BundleContents {
            vault_id: "vault".into(),
            generation_id: "generation".into(),
            device_id: "remote".into(),
            start_seq: seq,
            end_seq: seq,
            previous_path: previous_end_seq.map(|end| format!("bundles/{end}.acmb")),
            previous_sha256: previous_end_seq.map(|_| "ab".repeat(32)),
            previous_end_seq,
            changes: vec![BundleChange {
                local_seq: seq,
                key,
                operation,
                version: EntityVersion::new(version, 0, "remote"),
                content_hash,
                snapshot,
            }],
        };
        DecodedBundle {
            header: BundleHeader {
                vault_id: contents.vault_id.clone(),
                generation_id: contents.generation_id.clone(),
                device_id: contents.device_id.clone(),
                start_seq: seq,
                end_seq: seq,
                previous_path: contents.previous_path.clone(),
                previous_sha256: contents.previous_sha256.clone(),
                previous_end_seq,
                compression: CompressionAlgorithm::Zstandard,
                protection: ProtectionAlgorithm::Plain,
                nonce: None,
                payload_length: 1,
                payload_sha256: "cd".repeat(32),
            },
            contents,
        }
    }

    fn anchor(seq: i64) -> RemoteObjectAnchor {
        RemoteObjectAnchor {
            end_seq: seq,
            path: format!(
                "v1/generations/generation/devices/remote/bundles/{seq}-{seq}-{}.acmb",
                "ab".repeat(32)
            ),
            sha256: "ab".repeat(32),
        }
    }

    #[tokio::test]
    async fn newer_delete_wins_older_update_is_ignored_without_outbox_echo() {
        let pool = pool().await;
        let engine = MergeEngine::new(pool.clone(), None);
        assert_eq!(
            engine
                .apply_bundle(
                    "generation",
                    "remote",
                    0,
                    &bundle(1, 10, MutationOperation::Upsert),
                    &anchor(1),
                )
                .await
                .unwrap()
                .applied,
            1
        );
        assert_eq!(
            engine
                .apply_bundle(
                    "generation",
                    "remote",
                    1,
                    &bundle(2, 11, MutationOperation::Delete),
                    &anchor(2),
                )
                .await
                .unwrap()
                .applied,
            1
        );
        assert_eq!(
            engine
                .apply_bundle(
                    "generation",
                    "remote",
                    2,
                    &bundle(3, 9, MutationOperation::Upsert),
                    &anchor(3),
                )
                .await
                .unwrap()
                .ignored,
            1
        );
        let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        let outbox: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_mutations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((sessions, outbox), (0, 0));
    }

    #[tokio::test]
    async fn equal_version_different_hash_is_rejected_and_duplicate_bundle_is_idempotent() {
        let pool = pool().await;
        let engine = MergeEngine::new(pool.clone(), None);
        let first = bundle(1, 10, MutationOperation::Upsert);
        engine
            .apply_bundle("generation", "remote", 0, &first, &anchor(1))
            .await
            .unwrap();
        assert_eq!(
            engine
                .apply_bundle("generation", "remote", 1, &first, &anchor(1))
                .await
                .unwrap()
                .ignored,
            1
        );
        let mut conflicting = bundle(2, 10, MutationOperation::Upsert);
        conflicting.contents.changes[0]
            .snapshot
            .as_mut()
            .unwrap()
            .title = "conflict".into();
        conflicting.contents.changes[0].content_hash = Some("ef".repeat(32));
        assert!(
            engine
                .apply_bundle("generation", "remote", 1, &conflicting, &anchor(2))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn upsert_snapshot_overwrite_clears_stale_embedding_chunks_and_vectors() {
        let pool = pool().await;
        sqlx::query("DROP TABLE IF EXISTS embedding_vec;")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE VIRTUAL TABLE embedding_vec USING vec0(
                chunk_id INTEGER PRIMARY KEY,
                embedding float[4] distance_metric=cosine,
                +session_id TEXT,
                +message_id TEXT,
                +platform TEXT
            );",
        )
        .execute(&pool)
        .await
        .unwrap();
        let engine = MergeEngine::new(pool.clone(), None);
        engine
            .apply_bundle(
                "generation",
                "remote",
                0,
                &bundle(1, 10, MutationOperation::Upsert),
                &anchor(1),
            )
            .await
            .unwrap();
        let session_id: String = sqlx::query_scalar("SELECT id FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO embedding_chunks
             (message_id, session_id, platform, chunk_index, role, text, content_hash,
              backend_id, model_id, dim, status, updated_at)
             VALUES ('m1', ?, 'chat', 0, 'user', 'stale text', 'hash', 'local', 'model', 4,
                     'ready', '2026-01-01T00:00:00Z')",
        )
        .bind(&session_id)
        .execute(&pool)
        .await
        .unwrap();
        let chunk_id: i64 = sqlx::query_scalar("SELECT id FROM embedding_chunks")
            .fetch_one(&pool)
            .await
            .unwrap();
        let embedding: Vec<u8> = [1.0f32, 2.0, 3.0, 4.0]
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        sqlx::query(
            "INSERT INTO embedding_vec(chunk_id, embedding, session_id, message_id, platform)
             VALUES (?, ?, ?, 'm1', 'chat')",
        )
        .bind(chunk_id)
        .bind(&embedding)
        .bind(&session_id)
        .execute(&pool)
        .await
        .unwrap();

        engine
            .apply_bundle(
                "generation",
                "remote",
                1,
                &bundle(2, 11, MutationOperation::Upsert),
                &anchor(2),
            )
            .await
            .unwrap();

        let chunks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM embedding_chunks")
            .fetch_one(&pool)
            .await
            .unwrap();
        let vectors: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM embedding_vec")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((chunks, vectors), (0, 0));
    }
}
use crate::{
    error::{AppError, Result},
    semantic::engine::SemanticEngine,
    sync::{
        bundle::DecodedBundle,
        hlc::HybridClock,
        store::{RemoteObjectAnchor, SyncStore, current_time_millis},
        types::{
            BundleChange, EntityKey, EntityVersion, MutationOperation, NormalizedSessionSnapshot,
        },
    },
};
use sqlx::{Sqlite, SqlitePool, Transaction};
use std::sync::Arc;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeOutcome {
    pub changed_session_ids: Vec<String>,
    pub deleted_session_ids: Vec<String>,
    pub applied: usize,
    pub ignored: usize,
}

pub struct MergeEngine {
    pool: SqlitePool,
    semantic: Option<Arc<SemanticEngine>>,
}

impl MergeEngine {
    pub fn new(pool: SqlitePool, semantic: Option<Arc<SemanticEngine>>) -> Self {
        Self { pool, semantic }
    }

    pub async fn apply_bundle(
        &self,
        generation_id: &str,
        source_device_id: &str,
        expected_cursor: i64,
        bundle: &DecodedBundle,
        anchor: &RemoteObjectAnchor,
    ) -> Result<MergeOutcome> {
        if bundle.header.generation_id != generation_id
            || bundle.header.device_id != source_device_id
            || bundle.contents.generation_id != generation_id
            || bundle.contents.device_id != source_device_id
        {
            return Err(AppError::InvalidData(
                "remote bundle identity mismatch".into(),
            ));
        }
        if anchor.end_seq != bundle.header.end_seq
            || anchor.path.is_empty()
            || anchor.sha256.len() != 64
            || !anchor.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(AppError::InvalidData(
                "remote bundle object anchor is invalid".into(),
            ));
        }
        let mut tx = self.pool.begin().await?;
        let current_cursor: Option<i64> = sqlx::query_scalar(
            "SELECT cursor_seq FROM sync_remote_cursors
             WHERE generation_id = ? AND remote_device_id = ?",
        )
        .bind(generation_id)
        .bind(source_device_id)
        .fetch_optional(&mut *tx)
        .await?;
        let current_cursor = current_cursor.unwrap_or(0);
        if current_cursor >= bundle.header.end_seq {
            return Ok(MergeOutcome {
                ignored: bundle.contents.changes.len(),
                ..MergeOutcome::default()
            });
        }
        if current_cursor != expected_cursor
            || bundle.header.start_seq != current_cursor + 1
            || bundle.header.previous_end_seq.unwrap_or(0) != current_cursor
        {
            return Err(AppError::InvalidData(
                "remote bundle cursor is not contiguous".into(),
            ));
        }

        let state = SyncStore::lock_device_state_in(&mut tx)
            .await?
            .ok_or_else(|| AppError::NotFound("sync device state".into()))?;
        let mut clock = HybridClock::new(
            state.device_id.clone(),
            state.hlc_wall_ms,
            state.hlc_counter,
        );
        let now_ms = current_time_millis();
        let mut outcome = MergeOutcome::default();
        for change in &bundle.contents.changes {
            clock
                .observe(&change.version, now_ms)
                .map_err(|error| AppError::InvalidData(error.to_string()))?;
            if let Some((existing_version, existing_operation, existing_hash)) =
                entity_version_in(&mut tx, &change.key).await?
            {
                match change.version.cmp(&existing_version) {
                    std::cmp::Ordering::Less => {
                        outcome.ignored += 1;
                        continue;
                    }
                    std::cmp::Ordering::Equal => {
                        if existing_operation == change.operation
                            && existing_hash == change.content_hash
                        {
                            outcome.ignored += 1;
                            continue;
                        }
                        return Err(AppError::InvalidData(
                            "remote equal-version content conflict".into(),
                        ));
                    }
                    std::cmp::Ordering::Greater => {}
                }
            }
            let session_id = match change.operation {
                MutationOperation::Upsert => {
                    let snapshot = change.snapshot.as_ref().ok_or_else(|| {
                        AppError::InvalidData("remote upsert snapshot is missing".into())
                    })?;
                    Some(upsert_snapshot_in(&mut tx, snapshot).await?)
                }
                MutationOperation::Delete => delete_by_key_in(&mut tx, &change.key).await?,
            };
            record_entity_version_in(&mut tx, change).await?;
            outcome.applied += 1;
            match (change.operation.clone(), session_id) {
                (MutationOperation::Upsert, Some(id)) => outcome.changed_session_ids.push(id),
                (MutationOperation::Delete, Some(id)) => outcome.deleted_session_ids.push(id),
                _ => {}
            }
        }
        sqlx::query(
            "UPDATE sync_device_state SET hlc_wall_ms = ?, hlc_counter = ? WHERE singleton = 1",
        )
        .bind(clock.state().0)
        .bind(clock.state().1)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO sync_remote_cursors(
               generation_id, remote_device_id, cursor_seq,
               anchor_end_seq, anchor_path, anchor_sha256, updated_at_ms
             )
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(generation_id, remote_device_id) DO UPDATE SET
               cursor_seq = excluded.cursor_seq,
               anchor_end_seq = excluded.anchor_end_seq,
               anchor_path = excluded.anchor_path,
               anchor_sha256 = excluded.anchor_sha256,
               updated_at_ms = excluded.updated_at_ms",
        )
        .bind(generation_id)
        .bind(source_device_id)
        .bind(bundle.header.end_seq)
        .bind(anchor.end_seq)
        .bind(&anchor.path)
        .bind(&anchor.sha256)
        .bind(now_ms)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        if let Some(semantic) = &self.semantic {
            for id in &outcome.changed_session_ids {
                if let Err(error) = semantic.request_session_index(id).await {
                    tracing::warn!(%error, session_id = %id, "remote session indexing failed");
                }
            }
            for id in &outcome.deleted_session_ids {
                if let Err(error) = semantic.delete_session(id).await {
                    tracing::warn!(%error, session_id = %id, "remote session index deletion failed");
                }
            }
        }
        Ok(outcome)
    }
}

async fn entity_version_in(
    tx: &mut Transaction<'_, Sqlite>,
    key: &EntityKey,
) -> Result<Option<(EntityVersion, MutationOperation, Option<String>)>> {
    Ok(
        sqlx::query_as::<_, (i64, i64, String, String, Option<String>)>(
            "SELECT version_wall_ms, version_counter, version_device_id, operation, content_hash
         FROM sync_entity_versions WHERE platform = ? AND platform_session_id = ?",
        )
        .bind(&key.platform)
        .bind(&key.platform_session_id)
        .fetch_optional(&mut **tx)
        .await?
        .map(|(wall, counter, device, operation, hash)| {
            (
                EntityVersion::new(wall, counter, device),
                if operation == "delete" {
                    MutationOperation::Delete
                } else {
                    MutationOperation::Upsert
                },
                hash,
            )
        }),
    )
}

async fn record_entity_version_in(
    tx: &mut Transaction<'_, Sqlite>,
    change: &BundleChange,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO sync_entity_versions
         (platform, platform_session_id, operation, version_wall_ms, version_counter,
          version_device_id, content_hash)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(platform, platform_session_id) DO UPDATE SET
           operation = excluded.operation, version_wall_ms = excluded.version_wall_ms,
           version_counter = excluded.version_counter, version_device_id = excluded.version_device_id,
           content_hash = excluded.content_hash",
    )
    .bind(&change.key.platform)
    .bind(&change.key.platform_session_id)
    .bind(if change.operation == MutationOperation::Delete { "delete" } else { "upsert" })
    .bind(change.version.wall_ms)
    .bind(change.version.counter)
    .bind(&change.version.device_id)
    .bind(&change.content_hash)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn upsert_snapshot_in(
    tx: &mut Transaction<'_, Sqlite>,
    snapshot: &NormalizedSessionSnapshot,
) -> Result<String> {
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT id FROM sessions WHERE platform = ? AND platform_session_id = ?",
    )
    .bind(&snapshot.key.platform)
    .bind(&snapshot.key.platform_session_id)
    .fetch_optional(&mut **tx)
    .await?;
    let id = existing.unwrap_or_else(|| format!("sync-{}", uuid::Uuid::new_v4()));
    // Mirror delete_by_key_in: an overwrite must drop the session's stale
    // embedding chunks and vectors, or stale vectors keep matching search
    // queries for messages that no longer exist.
    let has_chunks_table: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'embedding_chunks')",
    )
    .fetch_one(&mut **tx)
    .await
    .unwrap_or(false);

    if has_chunks_table {
        let chunk_ids: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM embedding_chunks WHERE session_id = ?")
                .bind(&id)
                .fetch_all(&mut **tx)
                .await?;
        crate::database::delete_embedding_vectors_in(&mut *tx, &chunk_ids).await?;
        sqlx::query("DELETE FROM embedding_chunks WHERE session_id = ?")
            .bind(&id)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query(
        "INSERT INTO sessions (id, platform, platform_session_id, title, created_at, updated_at, imported_at, raw_data)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(platform, platform_session_id) DO UPDATE SET title=excluded.title,
           created_at=excluded.created_at, updated_at=excluded.updated_at,
           imported_at=excluded.imported_at, raw_data=excluded.raw_data",
    )
    .bind(&id)
    .bind(&snapshot.key.platform)
    .bind(&snapshot.key.platform_session_id)
    .bind(&snapshot.title)
    .bind(&snapshot.created_at)
    .bind(&snapshot.updated_at)
    .bind(&snapshot.imported_at)
    .bind(serde_json::to_string(&snapshot.raw_data)?)
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM messages WHERE session_id = ?")
        .bind(&id)
        .execute(&mut **tx)
        .await?;
    for (seq, message) in snapshot.messages.iter().enumerate() {
        sqlx::query("INSERT INTO messages (id, session_id, role, content, metadata, created_at, seq) VALUES (?, ?, ?, ?, ?, ?, ?)")
            .bind(format!("{id}_{seq}"))
            .bind(&id)
            .bind(&message.role)
            .bind(&message.content)
            .bind(serde_json::to_string(&message.metadata)?)
            .bind(&message.created_at)
            .bind(seq as i64)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query(
        "INSERT INTO session_fts_ids(session_id) VALUES (?) ON CONFLICT(session_id) DO NOTHING",
    )
    .bind(&id)
    .execute(&mut **tx)
    .await?;
    let fts_rowid: i64 =
        sqlx::query_scalar("SELECT fts_rowid FROM session_fts_ids WHERE session_id = ?")
            .bind(&id)
            .fetch_one(&mut **tx)
            .await?;
    sqlx::query("DELETE FROM session_fts WHERE rowid = ?")
        .bind(fts_rowid)
        .execute(&mut **tx)
        .await?;
    let content = snapshot
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    sqlx::query("INSERT INTO session_fts(rowid, session_id, title, content) VALUES (?, ?, ?, ?)")
        .bind(fts_rowid)
        .bind(&id)
        .bind(&snapshot.title)
        .bind(content)
        .execute(&mut **tx)
        .await?;
    Ok(id)
}

async fn delete_by_key_in(
    tx: &mut Transaction<'_, Sqlite>,
    key: &EntityKey,
) -> Result<Option<String>> {
    let id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM sessions WHERE platform = ? AND platform_session_id = ?",
    )
    .bind(&key.platform)
    .bind(&key.platform_session_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(id) = id else { return Ok(None) };
    if let Some(rowid) =
        sqlx::query_scalar::<_, i64>("SELECT fts_rowid FROM session_fts_ids WHERE session_id = ?")
            .bind(&id)
            .fetch_optional(&mut **tx)
            .await?
    {
        sqlx::query("DELETE FROM session_fts WHERE rowid = ?")
            .bind(rowid)
            .execute(&mut **tx)
            .await?;
    }
    let has_chunks_table: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'embedding_chunks')",
    )
    .fetch_one(&mut **tx)
    .await
    .unwrap_or(false);

    if has_chunks_table {
        let chunk_ids: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM embedding_chunks WHERE session_id = ?")
                .bind(&id)
                .fetch_all(&mut **tx)
                .await?;
        crate::database::delete_embedding_vectors_in(&mut *tx, &chunk_ids).await?;
        sqlx::query("DELETE FROM embedding_chunks WHERE session_id = ?")
            .bind(&id)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(&id)
        .execute(&mut **tx)
        .await?;
    Ok(Some(id))
}
