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
///
/// History:
/// - v1: initial release.
/// - v2: `ConnectParams` and `PathParams` renamed `from`/`to` to
///   `from_key`/`to_key` for consistency with `Edge` and the other `*Params`
///   `topic_key` convention. All `*Params` types now `deny_unknown_fields`.
/// - v3: added `BeginEditorSession`/`EndEditorSession`/`EditorSessionStatus`
///   request variants, the `EditorBusy` typed error, and the optional
///   `error_data` field on `CairnResponse` for transporting structured
///   error payloads (used by `EditorBusy` to carry `since` and `reason`).
///   The bump is additive — old daemons reject the new variants cleanly,
///   new daemons still understand v2 requests.
/// - v4: added `SetTags`, `Disconnect`, and `MoveBlock` request variants.
pub const RPC_PROTOCOL_VERSION: u32 = 4;

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

    // New ops (v4+)
    BatchRewrite(BatchRewriteParams),
    SetSummary(SetSummaryParams),
    SetTags(SetTagsParams),
    Disconnect(DisconnectParams),
    DeleteBlock(DeleteBlockParams),
    MoveBlock(MoveBlockParams),

    // Topic lock (v5)
    LockTopic { key: String },
    UnlockTopic { key: String },

    // Editor session control (v3)
    BeginEditorSession(BeginEditorSessionParams),
    EndEditorSession,
    EditorSessionStatus,
}

impl CairnRequest {
    /// True if this request is a *graph mutation* — i.e. it would change
    /// state visible to other clients on a subsequent read. Used by the
    /// daemon to decide whether the editor-session lock applies. Reads
    /// (`prime`, `search`, `stats`, `graph_status`, `snapshot` to disk,
    /// `export_json`, etc.) bypass the lock and stay available so an
    /// agent can keep priming context while the user is curating.
    ///
    /// Editor-session control RPCs (`BeginEditorSession`, `EndEditorSession`,
    /// `EditorSessionStatus`) are deliberately *not* mutations — they
    /// manage the lock itself and must always be reachable.
    pub fn is_mutation(&self) -> bool {
        use CairnRequest::*;
        match self {
            // Mutations: change graph state.
            InitDefaults { .. }
            | Learn(_)
            | Connect(_)
            | Amend(_)
            | Forget(_)
            | Rewrite(_)
            | Rename(_)
            | Reset
            | Checkpoint(_)
            | SetVoice { .. }
            | SetPreferences { .. }
            | Restore(_)
            | ImportJson { .. }
            | BatchRewrite(_)
            | SetSummary(_)
            | SetTags(_)
            | Disconnect(_)
            | DeleteBlock(_)
            | MoveBlock(_)
            | LockTopic { .. }
            | UnlockTopic { .. } => true,

            // Reads: do not change graph state. `Snapshot` and `ExportJson`
            // produce files but never modify the live graph, so they're
            // safe under an editor lock — and useful (snapshot before
            // risky edits).
            Ping
            | SchemaVersion
            | DbPath
            | History(_)
            | GetTopic { .. }
            | Search(_)
            | Explore(_)
            | Path(_)
            | Nearby(_)
            | Stats
            | GraphView
            | Prime(_)
            | GraphStatus
            | GetVoice
            | GetPreferences
            | Snapshot(_)
            | ExportJson
            | ListSnapshots => false,

            // Editor-session control: always reachable, never blocked.
            BeginEditorSession(_) | EndEditorSession | EditorSessionStatus => false,
        }
    }
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
    /// On error: optional structured payload that lets the client rebuild
    /// error variants with non-string fields (currently `EditorBusy`'s
    /// `since`/`reason`). `#[serde(default)]` so older clients/daemons
    /// without this field still round-trip. `skip_serializing_if` keeps
    /// the wire format clean for the common case where there's no payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_data: Option<serde_json::Value>,
}

impl CairnResponse {
    pub fn ok_value(value: serde_json::Value) -> Self {
        Self {
            ok: true,
            result: Some(value),
            error: None,
            error_kind: None,
            error_data: None,
        }
    }

    pub fn ok_unit() -> Self {
        Self {
            ok: true,
            result: Some(serde_json::Value::Null),
            error: None,
            error_kind: None,
            error_data: None,
        }
    }

    pub fn err(e: &CairnError) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(e.to_string()),
            error_kind: Some(classify(e).into()),
            error_data: error_data(e),
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
        TopicLocked(_) => "topic_locked",
        SchemaVersionMismatch { .. } => "schema_version_mismatch",
        EditorBusy { .. } => "editor_busy",
        Io(_) => "io",
        Other(_) => "other",
    }
}

/// Serialize the structured fields of error variants whose information
/// would otherwise be lost in the stringified `error` message. Currently
/// only `EditorBusy` carries data here; other variants return `None`.
fn error_data(e: &CairnError) -> Option<serde_json::Value> {
    match e {
        CairnError::EditorBusy { since, reason } => Some(serde_json::json!({
            "since": since,
            "reason": reason,
        })),
        _ => None,
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
            extra_blocks: vec![],
        }));
        round_trip(CairnRequest::Connect(ConnectParams {
            from_key: "a".into(),
            to_key: "b".into(),
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
            from_key: "a".into(),
            to_key: "b".into(),
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
        round_trip(CairnRequest::BeginEditorSession(BeginEditorSessionParams {
            reason: Some("manual triage".into()),
        }));
        round_trip(CairnRequest::BeginEditorSession(BeginEditorSessionParams {
            reason: None,
        }));
        round_trip(CairnRequest::EndEditorSession);
        round_trip(CairnRequest::EditorSessionStatus);
    }

    #[test]
    fn response_round_trip() {
        let ok = CairnResponse::ok_value(serde_json::json!({"x": 1}));
        let s = serde_json::to_string(&ok).unwrap();
        let _: CairnResponse = serde_json::from_str(&s).unwrap();

        let err = CairnResponse::err(&CairnError::TopicNotFound("billing".into()));
        assert_eq!(err.error_kind.as_deref(), Some("topic_not_found"));
        assert!(err.error_data.is_none());
    }

    #[test]
    fn editor_busy_response_carries_structured_data() {
        let now = chrono::Utc::now();
        let resp = CairnResponse::err(&CairnError::EditorBusy {
            since: now,
            reason: Some("manual triage".into()),
        });
        assert_eq!(resp.error_kind.as_deref(), Some("editor_busy"));
        let data = resp.error_data.clone().expect("error_data populated");
        assert_eq!(data["reason"], "manual triage");
        // Round-trip through JSON to make sure it survives the wire.
        let line = serde_json::to_string(&resp).unwrap();
        let back: CairnResponse = serde_json::from_str(&line).unwrap();
        let back_data = back.error_data.expect("error_data survived");
        assert_eq!(back_data["reason"], "manual triage");
    }

    #[test]
    fn old_response_format_without_error_data_still_decodes() {
        // Pre-v3 daemons emit responses with no `error_data` field at all.
        // The new client must accept them via the serde default.
        let legacy = serde_json::json!({
            "ok": false,
            "result": null,
            "error": "Topic not found: billing",
            "error_kind": "topic_not_found",
        });
        let resp: CairnResponse = serde_json::from_value(legacy).unwrap();
        assert!(!resp.ok);
        assert!(resp.error_data.is_none());
    }

    #[test]
    fn is_mutation_classification() {
        // Spot-check the classifier so a future variant can't silently
        // slip into the wrong bucket without tripping a test.
        assert!(CairnRequest::Learn(LearnParams {
            topic_key: "k".into(),
            title: None,
            summary: None,
            content: "c".into(),
            voice: None,
            tags: vec![],
            position: Position::End,
            extra_blocks: vec![],
        })
        .is_mutation());
        assert!(CairnRequest::Reset.is_mutation());
        assert!(CairnRequest::ImportJson { json: "{}".into() }.is_mutation());
        assert!(!CairnRequest::Stats.is_mutation());
        assert!(!CairnRequest::Search(SearchParams::default()).is_mutation());
        assert!(!CairnRequest::Snapshot(SnapshotParams {
            name: None,
            path: None,
        })
        .is_mutation());
        assert!(!CairnRequest::ExportJson.is_mutation());
        // Editor-session control is never a mutation — must always be reachable.
        assert!(
            !CairnRequest::BeginEditorSession(BeginEditorSessionParams { reason: None })
                .is_mutation()
        );
        assert!(!CairnRequest::EndEditorSession.is_mutation());
        assert!(!CairnRequest::EditorSessionStatus.is_mutation());
    }
}
