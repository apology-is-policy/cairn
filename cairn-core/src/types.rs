use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

// ── Core data types ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topic {
    pub key: String,
    pub title: String,
    pub summary: String,
    pub blocks: Vec<Block>,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deprecated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub id: String,
    pub content: String,
    pub voice: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from_key: String,
    pub to_key: String,
    pub kind: EdgeKind,
    pub note: String,
    pub severity: Option<Severity>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    DependsOn,
    Contradicts,
    ReplacedBy,
    Gotcha,
    SeeAlso,
    WarStory,
    Owns,
}

impl EdgeKind {
    pub fn table_name(&self) -> &'static str {
        match self {
            Self::DependsOn => "depends_on",
            Self::Contradicts => "contradicts",
            Self::ReplacedBy => "replaced_by",
            Self::Gotcha => "gotcha",
            Self::SeeAlso => "see_also",
            Self::WarStory => "war_story",
            Self::Owns => "owns",
        }
    }

    pub const ALL: &[EdgeKind] = &[
        Self::DependsOn,
        Self::Contradicts,
        Self::ReplacedBy,
        Self::Gotcha,
        Self::SeeAlso,
        Self::WarStory,
        Self::Owns,
    ];

    pub fn from_table_name(name: &str) -> Option<Self> {
        match name {
            "depends_on" => Some(Self::DependsOn),
            "contradicts" => Some(Self::Contradicts),
            "replaced_by" => Some(Self::ReplacedBy),
            "gotcha" => Some(Self::Gotcha),
            "see_also" => Some(Self::SeeAlso),
            "war_story" => Some(Self::WarStory),
            "owns" => Some(Self::Owns),
            _ => None,
        }
    }
}

impl fmt::Display for EdgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.table_name())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Low => f.write_str("low"),
            Self::Medium => f.write_str("medium"),
            Self::High => f.write_str("high"),
            Self::Critical => f.write_str("critical"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEvent {
    pub op: String,
    pub target: String,
    pub detail: String,
    pub diff: Option<String>,
    pub session_id: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Voice {
    pub content: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preferences {
    pub prime_max_tokens: i64,
    pub prime_include_gotchas: bool,
    pub learn_verbosity: String,
    pub learn_auto: bool,
    pub updated_at: DateTime<Utc>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            prime_max_tokens: 4000,
            prime_include_gotchas: true,
            learn_verbosity: "normal".into(),
            learn_auto: true,
            updated_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Position {
    Start,
    End,
    After(String),
}

// ── Operation parameters ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LearnParams {
    pub topic_key: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub content: String,
    pub voice: Option<String>,
    pub tags: Vec<String>,
    pub position: Position,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectParams {
    pub from_key: String,
    pub to_key: String,
    pub edge_type: EdgeKind,
    pub note: String,
    pub severity: Option<Severity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AmendParams {
    pub topic_key: String,
    pub block_id: String,
    pub new_content: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchParams {
    pub query: String,
    pub expand: bool,
    pub limit: usize,
}

impl Default for SearchParams {
    fn default() -> Self {
        Self {
            query: String::new(),
            expand: true,
            limit: 10,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExploreParams {
    pub topic_key: String,
    pub depth: usize,
    pub edge_types: Vec<EdgeKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathParams {
    pub from_key: String,
    pub to_key: String,
    pub max_depth: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NearbyParams {
    pub topic_key: String,
    pub hops: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointParams {
    pub session_id: String,
    pub emergency: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotParams {
    pub name: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreParams {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgetParams {
    pub topic_key: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RewriteParams {
    pub topic_key: String,
    pub new_blocks: Vec<NewBlock>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewBlock {
    pub content: String,
    pub voice: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryParams {
    pub topic_key: Option<String>,
    pub limit: usize,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrimeParams {
    pub task: String,
    pub max_tokens: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenameParams {
    pub old_key: String,
    pub new_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetTagsParams {
    pub topic_key: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetSummaryParams {
    pub topic_key: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisconnectParams {
    pub from_key: String,
    pub to_key: String,
    pub edge_type: EdgeKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveBlockParams {
    pub topic_key: String,
    pub block_id: String,
    pub position: Position,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeginEditorSessionParams {
    /// Optional human-readable label for what the user is doing in this
    /// session. Surfaced to other clients via `EditorSessionStatus` and in
    /// the `EditorBusy` error so an agent can explain *why* it's blocked.
    pub reason: Option<String>,
}

/// Snapshot of the daemon's editor-session state. Returned by
/// `editor_session_status()`. `None` means no client is currently holding
/// the lock.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorSessionInfo {
    pub since: DateTime<Utc>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", content = "content", rename_all = "snake_case")]
pub enum VoiceAction {
    Read,
    Update(String),
}

// ── Operation results ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnResult {
    pub topic_key: String,
    pub block_id: String,
    pub action: String,
    pub topic_block_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectResult {
    pub edge: String,
    pub from: String,
    pub to: String,
    pub action: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmendResult {
    pub topic_key: String,
    pub block_id: String,
    pub action: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResultItem {
    pub topic_key: String,
    pub title: String,
    pub summary: String,
    pub score: f64,
    pub neighbors: Vec<NeighborSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeighborSummary {
    pub key: String,
    pub edge: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub results: Vec<SearchResultItem>,
    pub total_matches: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExploreResult {
    pub center: String,
    pub nodes: Vec<NodeSummary>,
    pub edges: Vec<EdgeSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSummary {
    pub key: String,
    pub title: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeSummary {
    pub from: String,
    pub to: String,
    pub edge_type: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathStep {
    pub node: Option<String>,
    pub edge: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathResult {
    pub found: bool,
    pub path: Vec<PathStep>,
    pub depth: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NearbyEntry {
    pub key: String,
    pub title: String,
    pub distance: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NearbyResult {
    pub center: String,
    pub by_edge_type: std::collections::HashMap<String, Vec<NearbyEntry>>,
    pub total_nodes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointResult {
    pub session_id: String,
    pub mutations_persisted: usize,
    pub emergency: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotResult {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreResult {
    pub restored_from: String,
    pub safety_snapshot: String,
    pub topics_restored: usize,
    pub edges_restored: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgetResult {
    pub topic_key: String,
    pub action: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewriteResult {
    pub topic_key: String,
    pub action: String,
    pub old_block_count: usize,
    pub new_block_count: usize,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenameResult {
    pub old_key: String,
    pub new_key: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetTagsResult {
    pub topic_key: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetSummaryResult {
    pub topic_key: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisconnectResult {
    pub edge: String,
    pub from: String,
    pub to: String,
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveBlockResult {
    pub topic_key: String,
    pub block_id: String,
    pub new_position: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphViewResult {
    pub topics: Vec<NodeSummary>,
    pub edges: Vec<EdgeSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryResult {
    pub events: Vec<HistoryEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicStats {
    pub total: usize,
    pub active: usize,
    pub deprecated: usize,
    pub stale_90d: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeStats {
    pub total: usize,
    pub by_type: std::collections::HashMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicRank {
    pub key: String,
    pub title: String,
    pub edge_count: Option<usize>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsResult {
    pub topics: TopicStats,
    pub edges: EdgeStats,
    pub most_connected: Vec<TopicRank>,
    pub recently_updated: Vec<TopicRank>,
    pub oldest_untouched: Vec<TopicRank>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphStatusResult {
    pub active: bool,
    pub db_path: String,
    pub stats: TopicStats,
    pub protocol: String,
    pub voice: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrimeResult {
    pub context: String,
    pub matched_topics: Vec<String>,
    pub related_topics: Vec<String>,
    pub token_estimate: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceResult {
    pub content: String,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T: Serialize + serde::de::DeserializeOwned + std::fmt::Debug>(value: T) {
        let json = serde_json::to_string(&value).expect("serialize");
        let _back: T = serde_json::from_str(&json).expect("deserialize");
    }

    #[test]
    fn position_round_trip() {
        round_trip(Position::Start);
        round_trip(Position::End);
        round_trip(Position::After("b_123".into()));
    }

    #[test]
    fn voice_action_round_trip() {
        round_trip(VoiceAction::Read);
        round_trip(VoiceAction::Update("hello".into()));
    }

    #[test]
    fn learn_params_round_trip() {
        round_trip(LearnParams {
            topic_key: "k".into(),
            title: Some("T".into()),
            summary: Some("S".into()),
            content: "C".into(),
            voice: Some("calm".into()),
            tags: vec!["a".into(), "b".into()],
            position: Position::After("b_1".into()),
        });
    }

    #[test]
    fn connect_params_round_trip() {
        round_trip(ConnectParams {
            from_key: "a".into(),
            to_key: "b".into(),
            edge_type: EdgeKind::DependsOn,
            note: "n".into(),
            severity: Some(Severity::High),
        });
    }

    #[test]
    fn connect_params_rejects_legacy_field_names() {
        // The pre-v2 wire shape used `from`/`to`. Make sure the new schema
        // refuses it cleanly with an unknown-field error rather than
        // silently ignoring the field and complaining about a missing one.
        let legacy = serde_json::json!({
            "from": "a",
            "to": "b",
            "edge_type": "depends_on",
            "note": "n",
            "severity": null,
        });
        let err = serde_json::from_value::<ConnectParams>(legacy).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown field `from`"),
            "expected unknown-field error, got: {msg}"
        );
    }

    #[test]
    fn all_param_types_round_trip() {
        round_trip(AmendParams {
            topic_key: "k".into(),
            block_id: "b".into(),
            new_content: "c".into(),
            reason: "r".into(),
        });
        round_trip(SearchParams::default());
        round_trip(ExploreParams {
            topic_key: "k".into(),
            depth: 2,
            edge_types: vec![EdgeKind::SeeAlso],
        });
        round_trip(PathParams {
            from_key: "a".into(),
            to_key: "b".into(),
            max_depth: 5,
        });
        round_trip(NearbyParams {
            topic_key: "k".into(),
            hops: 2,
        });
        round_trip(CheckpointParams {
            session_id: "s".into(),
            emergency: false,
        });
        round_trip(SnapshotParams {
            name: Some("n".into()),
            path: None,
        });
        round_trip(RestoreParams { name: "n".into() });
        round_trip(ForgetParams {
            topic_key: "k".into(),
            reason: "r".into(),
        });
        round_trip(RewriteParams {
            topic_key: "k".into(),
            new_blocks: vec![NewBlock {
                content: "c".into(),
                voice: None,
            }],
            reason: "r".into(),
        });
        round_trip(HistoryParams {
            topic_key: Some("k".into()),
            limit: 10,
            session_id: None,
        });
        round_trip(PrimeParams {
            task: "t".into(),
            max_tokens: Some(2000),
        });
        round_trip(RenameParams {
            old_key: "a".into(),
            new_key: "b".into(),
        });
    }
}
