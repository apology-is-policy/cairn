use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::ErrorData as McpError;
use rmcp::model::*;
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use serde::{Deserialize, Deserializer};

use cairn_core::{default_db_path, CairnClient};

/// Defensive deserializer for `Vec<String>` fields that some MCP clients send
/// as a stringified JSON array (e.g. `"[\"a\",\"b\"]"`) instead of a real array.
/// Also accepts a comma-separated string. Falls back to `None` for empty input.
fn flexible_string_vec<'de, D>(d: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Vec(Vec<String>),
        Str(String),
    }

    let opt: Option<Either> = Option::deserialize(d)?;
    match opt {
        None => Ok(None),
        Some(Either::Vec(v)) => Ok(Some(v)),
        Some(Either::Str(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            // Try a JSON-encoded array first.
            if let Ok(v) = serde_json::from_str::<Vec<String>>(trimmed) {
                return Ok(Some(v));
            }
            // Fall back to comma-separated.
            Ok(Some(
                trimmed
                    .split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect(),
            ))
        }
    }
}

// ── Parameter types ──────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrimeRequest {
    /// Natural language task description, ticket ID, or topic keys
    pub task: String,
    /// Optional override for max context tokens
    pub max_tokens: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LearnRequest {
    /// Existing key to append to, or new key to create
    pub topic_key: String,
    /// Title (used only when creating a new topic)
    pub title: Option<String>,
    /// Summary for FTS search. Auto-generated from content if not provided on new topics.
    pub summary: Option<String>,
    /// The insight, in the developer's voice
    pub content: String,
    /// Optional mood/tone annotation
    pub voice: Option<String>,
    /// Tags for categorization
    #[serde(default, deserialize_with = "flexible_string_vec")]
    pub tags: Option<Vec<String>>,
    /// Position: "start", "end", or "after:<block_id>"
    pub position: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectRequest {
    /// Source topic key
    pub from_key: String,
    /// Target topic key
    pub to_key: String,
    /// Edge type: depends_on, contradicts, replaced_by, gotcha, see_also, war_story, owns
    pub edge_type: String,
    /// Why this connection exists
    pub note: String,
    /// Severity for gotcha edges: low, medium, high, critical
    pub severity: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AmendRequest {
    /// Topic key
    pub topic_key: String,
    /// Block ID to amend
    pub block_id: String,
    /// Corrected content
    pub new_content: String,
    /// Reason for amendment
    pub reason: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    /// Natural language search query
    pub query: String,
    /// Include 1-hop neighbors (default: true)
    pub expand: Option<bool>,
    /// Max topics to return (default: 10)
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExploreRequest {
    /// Topic key to explore from
    pub topic_key: String,
    /// Traversal depth (default: 1)
    pub depth: Option<usize>,
    /// Edge type filter (empty = all)
    #[serde(default, deserialize_with = "flexible_string_vec")]
    pub edge_types: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PathRequest {
    /// Source topic key
    pub from_key: String,
    /// Target topic key
    pub to_key: String,
    /// Max hops (default: 5)
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NearbyRequest {
    /// Topic key
    pub topic_key: String,
    /// Traversal distance (default: 2)
    pub hops: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckpointRequest {
    /// Session identifier
    pub session_id: String,
    /// Emergency flush (default: false)
    pub emergency: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SnapshotRequest {
    /// Human-readable name
    pub name: Option<String>,
    /// Override output directory
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreRequest {
    /// Snapshot name to restore from
    pub name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForgetRequest {
    /// Topic key to deprecate
    pub topic_key: String,
    /// Reason for deprecation
    pub reason: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameRequest {
    /// Current topic key
    pub old_key: String,
    /// New topic key
    pub new_key: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RewriteRequest {
    /// Topic key
    pub topic_key: String,
    /// New content blocks
    pub new_blocks: Vec<NewBlockRequest>,
    /// Reason for rewrite
    pub reason: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewBlockRequest {
    /// Block content
    pub content: String,
    /// Optional voice/tone
    pub voice: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoryRequest {
    /// Filter to a specific topic (optional)
    pub topic_key: Option<String>,
    /// Max events (default: 20)
    pub limit: Option<usize>,
    /// Filter to a session (optional)
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VoiceRequest {
    /// "read" or "update"
    pub action: String,
    /// New voice content (only for "update")
    pub content: Option<String>,
}

// ── Server ───────────────────────────────────────────────────────

#[derive(Clone)]
pub struct CairnMcpServer {
    cairn: Arc<CairnClient>,
    tool_router: ToolRouter<Self>,
}

fn parse_position(s: Option<&str>) -> cairn_core::Position {
    let s = match s {
        Some(s) => s.trim(),
        None => return cairn_core::Position::End,
    };
    if s.is_empty() {
        return cairn_core::Position::End;
    }
    // Simple string forms — what the docstring advertises.
    match s {
        "start" => return cairn_core::Position::Start,
        "end" => return cairn_core::Position::End,
        _ => {}
    }
    if let Some(after) = s.strip_prefix("after:") {
        return cairn_core::Position::After(after.to_string());
    }
    // Defensive: some MCP clients stringify the structured form
    // (`{"kind":"end"}` or `{"kind":"after","value":"b_..."}`).
    if let Ok(p) = serde_json::from_str::<cairn_core::Position>(s) {
        return p;
    }
    cairn_core::Position::End
}

fn parse_edge_kind(s: &str) -> Result<cairn_core::EdgeKind, McpError> {
    cairn_core::EdgeKind::from_table_name(s)
        .ok_or_else(|| McpError::invalid_params(format!("Invalid edge type: {s}"), None))
}

fn parse_severity(s: &str) -> cairn_core::types::Severity {
    match s {
        "low" => cairn_core::Severity::Low,
        "high" => cairn_core::Severity::High,
        "critical" => cairn_core::Severity::Critical,
        _ => cairn_core::Severity::Medium,
    }
}

fn to_json_content(val: &impl serde::Serialize) -> Result<CallToolResult, McpError> {
    let json = serde_json::to_string_pretty(val)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
    Ok(CallToolResult::success(vec![Content::text(json)]))
}

fn cairn_err(e: cairn_core::CairnError) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

#[tool_router]
impl CairnMcpServer {
    pub fn new(cairn: Arc<CairnClient>) -> Self {
        Self {
            cairn,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Returns graph status, stats, behavioral contract, and voice. Call this first."
    )]
    async fn graph_status(&self) -> Result<CallToolResult, McpError> {
        let result = self.cairn.graph_status().await.map_err(cairn_err)?;
        to_json_content(&result)
    }

    #[tool(description = "Compose relevant context for a task. Call at the start of every task.")]
    async fn prime(
        &self,
        Parameters(req): Parameters<PrimeRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .cairn
            .prime(cairn_core::PrimeParams {
                task: req.task,
                max_tokens: req.max_tokens,
            })
            .await
            .map_err(cairn_err)?;
        to_json_content(&result)
    }

    #[tool(description = "Record a new insight or extend an existing topic.")]
    async fn learn(
        &self,
        Parameters(req): Parameters<LearnRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .cairn
            .learn(cairn_core::LearnParams {
                topic_key: req.topic_key,
                title: req.title,
                summary: req.summary,
                content: req.content,
                voice: req.voice,
                tags: req.tags.unwrap_or_default(),
                position: parse_position(req.position.as_deref()),
            })
            .await
            .map_err(cairn_err)?;
        to_json_content(&result)
    }

    #[tool(description = "Create a typed edge between two topics.")]
    async fn connect(
        &self,
        Parameters(req): Parameters<ConnectRequest>,
    ) -> Result<CallToolResult, McpError> {
        let kind = parse_edge_kind(&req.edge_type)?;
        let result = self
            .cairn
            .connect_topics(cairn_core::ConnectParams {
                from_key: req.from_key,
                to_key: req.to_key,
                edge_type: kind,
                note: req.note,
                severity: req.severity.as_deref().map(parse_severity),
            })
            .await
            .map_err(cairn_err)?;
        to_json_content(&result)
    }

    #[tool(description = "Correct or update a specific block within a topic.")]
    async fn amend(
        &self,
        Parameters(req): Parameters<AmendRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .cairn
            .amend(cairn_core::AmendParams {
                topic_key: req.topic_key,
                block_id: req.block_id,
                new_content: req.new_content,
                reason: req.reason,
            })
            .await
            .map_err(cairn_err)?;
        to_json_content(&result)
    }

    #[tool(description = "Full-text search across all topic content.")]
    async fn search(
        &self,
        Parameters(req): Parameters<SearchRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .cairn
            .search(cairn_core::SearchParams {
                query: req.query,
                expand: req.expand.unwrap_or(true),
                limit: req.limit.unwrap_or(10),
            })
            .await
            .map_err(cairn_err)?;
        to_json_content(&result)
    }

    #[tool(description = "Show all edges and neighbors of a topic.")]
    async fn explore(
        &self,
        Parameters(req): Parameters<ExploreRequest>,
    ) -> Result<CallToolResult, McpError> {
        let edge_types: Vec<cairn_core::EdgeKind> = req
            .edge_types
            .unwrap_or_default()
            .iter()
            .filter_map(|s| cairn_core::EdgeKind::from_table_name(s))
            .collect();

        let result = self
            .cairn
            .explore(cairn_core::ExploreParams {
                topic_key: req.topic_key,
                depth: req.depth.unwrap_or(1),
                edge_types,
            })
            .await
            .map_err(cairn_err)?;
        to_json_content(&result)
    }

    #[tool(description = "Find how two topics are connected through the graph.")]
    async fn path(
        &self,
        Parameters(req): Parameters<PathRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .cairn
            .path(cairn_core::PathParams {
                from_key: req.from_key,
                to_key: req.to_key,
                max_depth: req.max_depth.unwrap_or(5),
            })
            .await
            .map_err(cairn_err)?;
        to_json_content(&result)
    }

    #[tool(description = "Return all topics within N hops, grouped by edge type.")]
    async fn nearby(
        &self,
        Parameters(req): Parameters<NearbyRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .cairn
            .nearby(cairn_core::NearbyParams {
                topic_key: req.topic_key,
                hops: req.hops.unwrap_or(2),
            })
            .await
            .map_err(cairn_err)?;
        to_json_content(&result)
    }

    #[tool(description = "Persist session state. Called by hooks, not typically by the agent.")]
    async fn checkpoint(
        &self,
        Parameters(req): Parameters<CheckpointRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .cairn
            .checkpoint(cairn_core::CheckpointParams {
                session_id: req.session_id,
                emergency: req.emergency.unwrap_or(false),
            })
            .await
            .map_err(cairn_err)?;
        to_json_content(&result)
    }

    #[tool(description = "Create a named full backup of the database.")]
    async fn snapshot(
        &self,
        Parameters(req): Parameters<SnapshotRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .cairn
            .snapshot(cairn_core::SnapshotParams {
                name: req.name,
                path: req.path,
            })
            .await
            .map_err(cairn_err)?;
        to_json_content(&result)
    }

    #[tool(description = "Restore the database from a named snapshot. Destructive.")]
    async fn restore(
        &self,
        Parameters(req): Parameters<RestoreRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .cairn
            .restore(cairn_core::RestoreParams { name: req.name })
            .await
            .map_err(cairn_err)?;
        to_json_content(&result)
    }

    #[tool(description = "Mark a topic as deprecated (soft delete).")]
    async fn forget(
        &self,
        Parameters(req): Parameters<ForgetRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .cairn
            .forget(cairn_core::ForgetParams {
                topic_key: req.topic_key,
                reason: req.reason,
            })
            .await
            .map_err(cairn_err)?;
        to_json_content(&result)
    }

    #[tool(description = "Rename a topic key. Edges are preserved automatically.")]
    async fn rename(
        &self,
        Parameters(req): Parameters<RenameRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .cairn
            .rename(cairn_core::RenameParams {
                old_key: req.old_key,
                new_key: req.new_key,
            })
            .await
            .map_err(cairn_err)?;
        to_json_content(&result)
    }

    #[tool(description = "Replace all blocks in a topic. For complete rewrites.")]
    async fn rewrite(
        &self,
        Parameters(req): Parameters<RewriteRequest>,
    ) -> Result<CallToolResult, McpError> {
        let new_blocks = req
            .new_blocks
            .into_iter()
            .map(|b| cairn_core::NewBlock {
                content: b.content,
                voice: b.voice,
            })
            .collect();

        let result = self
            .cairn
            .rewrite(cairn_core::RewriteParams {
                topic_key: req.topic_key,
                new_blocks,
                reason: req.reason,
            })
            .await
            .map_err(cairn_err)?;
        to_json_content(&result)
    }

    #[tool(description = "Show the mutation log for a topic or the entire graph.")]
    async fn history(
        &self,
        Parameters(req): Parameters<HistoryRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .cairn
            .history(cairn_core::HistoryParams {
                topic_key: req.topic_key,
                limit: req.limit.unwrap_or(20),
                session_id: req.session_id,
            })
            .await
            .map_err(cairn_err)?;
        to_json_content(&result)
    }

    #[tool(description = "Graph overview: node counts, edge counts, most connected topics.")]
    async fn stats(&self) -> Result<CallToolResult, McpError> {
        let result = self.cairn.stats().await.map_err(cairn_err)?;
        to_json_content(&result)
    }

    #[tool(description = "Read or update the developer's voice/personality node.")]
    async fn voice(
        &self,
        Parameters(req): Parameters<VoiceRequest>,
    ) -> Result<CallToolResult, McpError> {
        match req.action.as_str() {
            "read" => {
                let voice = self.cairn.get_voice().await.map_err(cairn_err)?;
                match voice {
                    Some(v) => to_json_content(&cairn_core::VoiceResult {
                        content: v.content,
                        updated_at: v.updated_at,
                    }),
                    None => Ok(CallToolResult::success(vec![Content::text(
                        r#"{"content": null}"#,
                    )])),
                }
            }
            "update" => {
                let content = req
                    .content
                    .ok_or_else(|| McpError::invalid_params("content required for update", None))?;
                let result = self.cairn.set_voice(&content).await.map_err(cairn_err)?;
                to_json_content(&result)
            }
            _ => Err(McpError::invalid_params(
                "action must be 'read' or 'update'",
                None,
            )),
        }
    }
}

#[tool_handler]
impl ServerHandler for CairnMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "Cairn is a personal knowledge graph for AI coding agents. \
                 Call graph_status first to get the behavioral contract.",
            )
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Log to stderr — stdout is the JSON-RPC channel
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing_subscriber::filter::LevelFilter::WARN.into()),
        )
        .init();

    // Determine DB path from args, env, or repo discovery
    let path = std::env::args()
        .position(|a| a == "--db")
        .and_then(|i| std::env::args().nth(i + 1))
        .map(PathBuf::from)
        .or_else(|| std::env::var("CAIRN_DB").ok().map(PathBuf::from))
        .unwrap_or_else(default_db_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let cairn = Arc::new(CairnClient::connect_or_spawn(&path).await?);
    let server = CairnMcpServer::new(cairn);

    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;

    Ok(())
}
