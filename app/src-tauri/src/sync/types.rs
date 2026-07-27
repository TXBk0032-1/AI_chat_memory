use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct EntityKey {
    pub platform: String,
    pub platform_session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(try_from = "EntityVersionWire")]
pub struct EntityVersion {
    pub wall_ms: i64,
    pub counter: i64,
    pub device_id: String,
}

#[derive(Deserialize)]
struct EntityVersionWire {
    wall_ms: i64,
    counter: i64,
    device_id: String,
}

impl TryFrom<EntityVersionWire> for EntityVersion {
    type Error = &'static str;

    fn try_from(value: EntityVersionWire) -> Result<Self, Self::Error> {
        if value.wall_ms < 0 {
            return Err("wall_ms must be non-negative");
        }
        if value.counter < 0 {
            return Err("counter must be non-negative");
        }
        if value.wall_ms == i64::MAX && value.counter == i64::MAX {
            return Err("wall_ms and counter cannot both equal i64::MAX");
        }

        Ok(Self::new(value.wall_ms, value.counter, value.device_id))
    }
}

impl EntityVersion {
    pub fn new(wall_ms: i64, counter: i64, device_id: impl Into<String>) -> Self {
        Self {
            wall_ms,
            counter,
            device_id: device_id.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MutationOperation {
    Upsert,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncMessageSnapshot {
    pub role: String,
    pub content: String,
    pub metadata: serde_json::Value,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NormalizedSessionSnapshot {
    pub key: EntityKey,
    pub title: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub imported_at: String,
    pub raw_data: serde_json::Value,
    pub messages: Vec<SyncMessageSnapshot>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncTrigger {
    Startup,
    Periodic,
    LocalMutation,
    Manual,
}

#[cfg(test)]
mod tests {
    use super::{EntityVersion, MutationOperation, NormalizedSessionSnapshot, SyncTrigger};
    use serde_json::json;

    #[test]
    fn entity_version_rejects_negative_wall_time() {
        let error = serde_json::from_value::<EntityVersion>(json!({
            "wall_ms": -1,
            "counter": 0,
            "device_id": "device-a"
        }))
        .unwrap_err();

        assert!(error.to_string().contains("wall_ms must be non-negative"));
    }

    #[test]
    fn entity_version_rejects_negative_counter() {
        let error = serde_json::from_value::<EntityVersion>(json!({
            "wall_ms": 0,
            "counter": -1,
            "device_id": "device-a"
        }))
        .unwrap_err();

        assert!(error.to_string().contains("counter must be non-negative"));
    }

    #[test]
    fn entity_version_rejects_exhausted_protocol_value() {
        let error = serde_json::from_value::<EntityVersion>(json!({
            "wall_ms": i64::MAX,
            "counter": i64::MAX,
            "device_id": "device-a"
        }))
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("wall_ms and counter cannot both equal i64::MAX")
        );
    }

    #[test]
    fn entity_version_accepts_each_non_exhausted_maximum() {
        let max_wall = serde_json::from_value::<EntityVersion>(json!({
            "wall_ms": i64::MAX,
            "counter": i64::MAX - 1,
            "device_id": "device-a"
        }))
        .unwrap();
        let max_counter = serde_json::from_value::<EntityVersion>(json!({
            "wall_ms": i64::MAX - 1,
            "counter": i64::MAX,
            "device_id": "device-b"
        }))
        .unwrap();

        assert_eq!(
            max_wall,
            EntityVersion::new(i64::MAX, i64::MAX - 1, "device-a")
        );
        assert_eq!(
            max_counter,
            EntityVersion::new(i64::MAX - 1, i64::MAX, "device-b")
        );
    }

    #[test]
    fn fixed_external_json_fixture_parses_each_snapshot_field() {
        const FIXTURE: &str = r#"{
            "key": {"platform": "chat", "platform_session_id": "session-1"},
            "title": "Fixture title",
            "created_at": "2026-07-27T09:00:00Z",
            "updated_at": null,
            "imported_at": "2026-07-27T10:01:00Z",
            "raw_data": {"source": "fixture", "revision": 3},
            "messages": [{
                "role": "assistant",
                "content": "fixture content",
                "metadata": {"model": "fixture-model"},
                "created_at": "2026-07-27T09:01:00Z"
            }]
        }"#;

        let snapshot: NormalizedSessionSnapshot = serde_json::from_str(FIXTURE).unwrap();

        assert_eq!(snapshot.key.platform, "chat");
        assert_eq!(snapshot.key.platform_session_id, "session-1");
        assert_eq!(snapshot.title, "Fixture title");
        assert_eq!(snapshot.created_at.as_deref(), Some("2026-07-27T09:00:00Z"));
        assert_eq!(snapshot.updated_at, None);
        assert_eq!(snapshot.imported_at, "2026-07-27T10:01:00Z");
        assert_eq!(
            snapshot.raw_data,
            json!({"source": "fixture", "revision": 3})
        );
        assert_eq!(snapshot.messages.len(), 1);
        assert_eq!(snapshot.messages[0].role, "assistant");
        assert_eq!(snapshot.messages[0].content, "fixture content");
        assert_eq!(
            snapshot.messages[0].metadata,
            json!({"model": "fixture-model"})
        );
        assert_eq!(
            snapshot.messages[0].created_at.as_deref(),
            Some("2026-07-27T09:01:00Z")
        );
    }

    #[test]
    fn enum_wire_literals_are_fixed_snake_case_values() {
        let mutation_fixtures = [
            (r#""upsert""#, MutationOperation::Upsert),
            (r#""delete""#, MutationOperation::Delete),
        ];
        for (fixture, expected) in mutation_fixtures {
            assert_eq!(
                serde_json::from_str::<MutationOperation>(fixture).unwrap(),
                expected
            );
            assert_eq!(serde_json::to_string(&expected).unwrap(), fixture);
        }

        let trigger_fixtures = [
            (r#""startup""#, SyncTrigger::Startup),
            (r#""periodic""#, SyncTrigger::Periodic),
            (r#""local_mutation""#, SyncTrigger::LocalMutation),
            (r#""manual""#, SyncTrigger::Manual),
        ];
        for (fixture, expected) in trigger_fixtures {
            assert_eq!(
                serde_json::from_str::<SyncTrigger>(fixture).unwrap(),
                expected
            );
            assert_eq!(serde_json::to_string(&expected).unwrap(), fixture);
        }
    }
}
