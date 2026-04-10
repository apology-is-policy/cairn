//! Wire protocol shared by `CairnClient` and `cairn-server`.
//!
//! Each request is one line of JSON terminated by `\n`. Each response is one
//! line of JSON terminated by `\n`. The envelope carries the result as an
//! opaque `serde_json::Value` — the client knows the expected type from the
//! request it sent and deserializes accordingly.

use serde::{Deserialize, Serialize};

use crate::error::CairnError;
use crate::types::*;

/// Bumped on any incompatible change to `CairnRequest`/`CairnResponse` shape.
pub const RPC_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", content = "params", rename_all = "snake_case")]
pub enum CairnRequest {
    // Meta
    Ping,
    SchemaVersion,
    DbPath,
    InitDefaults { initial_voice: Option<String> },

    // Mutations
    Learn(LearnParams),
    Connect(ConnectParams),
    Amend(AmendParams),
    Forget(ForgetParams),
    Rewrite(RewriteParams),
    Rename(RenameParams),
    Reset,
    Checkpoint(CheckpointParams),
    History(HistoryParams),
    GetTopic { key: String },

    // Queries
    Search(SearchParams),
    Explore(ExploreParams),
    Path(PathParams),
    Nearby(NearbyParams),
    Stats,
    GraphView,

    // Context
    Prime(PrimeParams),
    GraphStatus,

    // Voice & preferences
    GetVoice,
    SetVoice { content: String },
    GetPreferences,
    SetPreferences { prefs: Preferences },

    // Snapshot
    Snapshot(SnapshotParams),
    Restore(RestoreParams),
    ExportJson,
    ImportJson { json: String },
    ListSnapshots,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CairnResponse {
    pub ok: bool,
    /// On success: result serialized as JSON.
    /// On error: null.
    pub result: Option<serde_json::Value>,
    /// On error: human-readable message.
    pub error: Option<String>,
    /// On error: classification kind so the client can rebuild a typed `CairnError`.
    pub error_kind: Option<String>,
}

impl CairnResponse {
    pub fn ok_value(value: serde_json::Value) -> Self {
        Self {
            ok: true,
            result: Some(value),
            error: None,
            error_kind: None,
        }
    }

    pub fn ok_unit() -> Self {
        Self {
            ok: true,
            result: Some(serde_json::Value::Null),
            error: None,
            error_kind: None,
        }
    }

    pub fn err(e: &CairnError) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(e.to_string()),
            error_kind: Some(classify(e).into()),
        }
    }
}

fn classify(e: &CairnError) -> &'static str {
    use CairnError::*;
    match e {
        Db(_) => "db",
        TopicNotFound(_) => "topic_not_found",
        BlockNotFound(_, _) => "block_not_found",
        SnapshotNotFound(_) => "snapshot_not_found",
        InvalidEdgeType(_) => "invalid_edge_type",
        EmptyContent(_) => "empty_content",
        TopicKeyConflict(_) => "topic_key_conflict",
        SchemaVersionMismatch { .. } => "schema_version_mismatch",
        Io(_) => "io",
        Other(_) => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(req: CairnRequest) {
        let s = serde_json::to_string(&req).expect("encode");
        let _back: CairnRequest = serde_json::from_str(&s).expect("decode");
    }

    #[test]
    fn request_round_trip_all_variants() {
        round_trip(CairnRequest::Ping);
        round_trip(CairnRequest::SchemaVersion);
        round_trip(CairnRequest::DbPath);
        round_trip(CairnRequest::InitDefaults {
            initial_voice: Some("hi".into()),
        });
        round_trip(CairnRequest::Learn(LearnParams {
            topic_key: "k".into(),
            title: None,
            summary: None,
            content: "c".into(),
            voice: None,
            tags: vec![],
            position: Position::End,
        }));
        round_trip(CairnRequest::Connect(ConnectParams {
            from: "a".into(),
            to: "b".into(),
            edge_type: EdgeKind::DependsOn,
            note: "n".into(),
            severity: None,
        }));
        round_trip(CairnRequest::Amend(AmendParams {
            topic_key: "k".into(),
            block_id: "b".into(),
            new_content: "c".into(),
            reason: "r".into(),
        }));
        round_trip(CairnRequest::Forget(ForgetParams {
            topic_key: "k".into(),
            reason: "r".into(),
        }));
        round_trip(CairnRequest::Rewrite(RewriteParams {
            topic_key: "k".into(),
            new_blocks: vec![],
            reason: "r".into(),
        }));
        round_trip(CairnRequest::Rename(RenameParams {
            old_key: "a".into(),
            new_key: "b".into(),
        }));
        round_trip(CairnRequest::Reset);
        round_trip(CairnRequest::Checkpoint(CheckpointParams {
            session_id: "s".into(),
            emergency: false,
        }));
        round_trip(CairnRequest::History(HistoryParams {
            topic_key: None,
            limit: 10,
            session_id: None,
        }));
        round_trip(CairnRequest::GetTopic { key: "k".into() });
        round_trip(CairnRequest::Search(SearchParams::default()));
        round_trip(CairnRequest::Explore(ExploreParams {
            topic_key: "k".into(),
            depth: 1,
            edge_types: vec![],
        }));
        round_trip(CairnRequest::Path(PathParams {
            from: "a".into(),
            to: "b".into(),
            max_depth: 5,
        }));
        round_trip(CairnRequest::Nearby(NearbyParams {
            topic_key: "k".into(),
            hops: 2,
        }));
        round_trip(CairnRequest::Stats);
        round_trip(CairnRequest::GraphView);
        round_trip(CairnRequest::Prime(PrimeParams {
            task: "t".into(),
            max_tokens: None,
        }));
        round_trip(CairnRequest::GraphStatus);
        round_trip(CairnRequest::GetVoice);
        round_trip(CairnRequest::SetVoice {
            content: "v".into(),
        });
        round_trip(CairnRequest::GetPreferences);
        round_trip(CairnRequest::SetPreferences {
            prefs: Preferences::default(),
        });
        round_trip(CairnRequest::Snapshot(SnapshotParams {
            name: None,
            path: None,
        }));
        round_trip(CairnRequest::Restore(RestoreParams { name: "n".into() }));
        round_trip(CairnRequest::ExportJson);
        round_trip(CairnRequest::ImportJson { json: "{}".into() });
        round_trip(CairnRequest::ListSnapshots);
    }

    #[test]
    fn response_round_trip() {
        let ok = CairnResponse::ok_value(serde_json::json!({"x": 1}));
        let s = serde_json::to_string(&ok).unwrap();
        let _: CairnResponse = serde_json::from_str(&s).unwrap();

        let err = CairnResponse::err(&CairnError::TopicNotFound("billing".into()));
        assert_eq!(err.error_kind.as_deref(), Some("topic_not_found"));
    }
}
