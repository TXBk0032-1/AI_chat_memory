use serde::{Deserialize, Deserializer, Serialize, de};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct EntityKey {
    pub platform: String,
    pub platform_session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct EntityVersion {
    #[serde(deserialize_with = "deserialize_non_negative_i64")]
    pub wall_ms: i64,
    #[serde(deserialize_with = "deserialize_non_negative_i64")]
    pub counter: i64,
    pub device_id: String,
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

fn deserialize_non_negative_i64<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = i64::deserialize(deserializer)?;
    if value < 0 {
        return Err(de::Error::custom("expected a non-negative i64"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::{
        EntityKey, EntityVersion, MutationOperation, NormalizedSessionSnapshot,
        SyncMessageSnapshot, SyncTrigger,
    };
    use serde_json::json;

    #[test]
    fn entity_version_rejects_negative_wall_time() {
        let result = serde_json::from_value::<EntityVersion>(json!({
            "wall_ms": -1,
            "counter": 0,
            "device_id": "device-a"
        }));

        assert!(result.is_err());
    }

    #[test]
    fn entity_version_rejects_negative_counter() {
        let result = serde_json::from_value::<EntityVersion>(json!({
            "wall_ms": 0,
            "counter": -1,
            "device_id": "device-a"
        }));

        assert!(result.is_err());
    }

    #[test]
    fn protocol_types_round_trip_with_snake_case_enums() {
        let snapshot = NormalizedSessionSnapshot {
            key: EntityKey {
                platform: "chat".into(),
                platform_session_id: "session-1".into(),
            },
            title: "Title".into(),
            created_at: None,
            updated_at: Some("2026-07-27T10:00:00Z".into()),
            imported_at: "2026-07-27T10:01:00Z".into(),
            raw_data: json!({ "source": "test" }),
            messages: vec![SyncMessageSnapshot {
                role: "user".into(),
                content: "hello".into(),
                metadata: json!({}),
                created_at: None,
            }],
        };

        let encoded = serde_json::to_value((
            &snapshot,
            EntityVersion::new(1, 2, "device-a"),
            MutationOperation::Upsert,
            SyncTrigger::LocalMutation,
        ))
        .unwrap();
        assert_eq!(encoded[2], "upsert");
        assert_eq!(encoded[3], "local_mutation");

        let decoded: (
            NormalizedSessionSnapshot,
            EntityVersion,
            MutationOperation,
            SyncTrigger,
        ) = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.0, snapshot);
        assert_eq!(decoded.1, EntityVersion::new(1, 2, "device-a"));
        assert_eq!(decoded.2, MutationOperation::Upsert);
        assert_eq!(decoded.3, SyncTrigger::LocalMutation);
    }
}
