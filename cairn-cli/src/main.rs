use std::path::PathBuf;

use clap::{Parser, Subcommand};

use cairn_core::*;

// Bundled agent files — embedded at compile time so the binary is self-contained.
const BUNDLED_AGENTS: &[(&str, &str)] = &[
    ("taxonomer.md", include_str!("../../agents/taxonomer.md")),
    (
        "taxonomer-explode.md",
        include_str!("../../agents/taxonomer-explode.md"),
    ),
    (
        "taxonomer-verify.md",
        include_str!("../../agents/taxonomer-verify.md"),
    ),
];

#[derive(Parser)]
#[command(name = "cairn", about = "Personal AI agent knowledge graph")]
struct Cli {
    /// Path to the Cairn database
    #[arg(long, env = "CAIRN_DB")]
    db: Option<String>,

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Initialize a new graph
    Init {
        /// Initial voice content
        #[arg(long)]
        voice: Option<String>,
        /// Taxonomy mode: "scan" to install taxonomer agent, or "describe Domain1, Domain2, ..." to create root topics
        #[arg(long)]
        taxonomy: Option<String>,
    },
    /// Show graph status and stats
    Status,
    /// Compose context for a task
    Prime {
        /// Task description
        task: String,
        /// Override max tokens
        #[arg(long)]
        max_tokens: Option<i64>,
    },
    /// Record an insight
    Learn {
        /// Topic key (e.g. "billing-retry")
        topic_key: String,
        /// The insight content
        content: String,
        /// Title for new topics
        #[arg(long)]
        title: Option<String>,
        /// Summary for FTS search (auto-generated from content if not provided)
        #[arg(long)]
        summary: Option<String>,
        /// Voice/mood annotation
        #[arg(long)]
        voice: Option<String>,
        /// Tags
        #[arg(long)]
        tag: Vec<String>,
        /// Position: start, end, or after:<block_id>
        #[arg(long, default_value = "end")]
        position: String,
    },
    /// Create a typed edge between topics
    Connect {
        /// Source topic key
        from: String,
        /// Target topic key
        to: String,
        /// Edge type: depends_on, contradicts, replaced_by, gotcha, see_also, war_story, owns
        edge_type: String,
        /// Why this connection exists
        #[arg(long)]
        note: Option<String>,
        /// Severity for gotcha edges: low, medium, high, critical
        #[arg(long)]
        severity: Option<String>,
    },
    /// Correct or update a block
    Amend {
        /// Topic key
        topic_key: String,
        /// Block ID to amend
        block_id: String,
        /// New content
        new_content: String,
        /// Reason for amendment
        #[arg(long)]
        reason: Option<String>,
    },
    /// Full-text search
    Search {
        /// Search query
        query: String,
        /// Include 1-hop neighbors
        #[arg(long, default_value = "true")]
        expand: bool,
        /// Max results
        #[arg(long, default_value = "10")]
        limit: usize,
    },
    /// Show edges and neighbors of a topic
    Explore {
        /// Topic key
        topic_key: String,
        /// Traversal depth
        #[arg(long, default_value = "1")]
        depth: usize,
    },
    /// Find connection path between topics
    Path {
        /// Source topic key
        from: String,
        /// Target topic key
        to: String,
        /// Max hops
        #[arg(long, default_value = "5")]
        max_depth: usize,
    },
    /// Show neighborhood grouped by edge type
    Nearby {
        /// Topic key
        topic_key: String,
        /// Traversal distance
        #[arg(long, default_value = "2")]
        hops: usize,
    },
    /// Persist session state
    Checkpoint {
        /// Session identifier
        #[arg(long)]
        session_id: Option<String>,
        /// Emergency flush
        #[arg(long)]
        emergency: bool,
    },
    /// Create a database snapshot
    Snapshot {
        /// Snapshot name
        #[arg(long)]
        name: Option<String>,
    },
    /// Restore from a snapshot
    Restore {
        /// Snapshot name
        name: String,
    },
    /// Deprecate a topic
    Forget {
        /// Topic key
        topic_key: String,
        /// Reason
        #[arg(long)]
        reason: Option<String>,
    },
    /// Rewrite a topic from stdin or file
    Rewrite {
        /// Topic key
        topic_key: String,
        /// Reason
        #[arg(long)]
        reason: Option<String>,
        /// Read content from file
        #[arg(long)]
        file: Option<String>,
    },
    /// Show mutation log
    History {
        /// Filter to a specific topic
        topic_key: Option<String>,
        /// Max events
        #[arg(long, default_value = "20")]
        limit: usize,
        /// Filter to a session
        #[arg(long)]
        session: Option<String>,
    },
    /// Rename a topic key (edges are preserved)
    Rename {
        /// Current topic key
        old_key: String,
        /// New topic key
        new_key: String,
    },
    /// Show the full graph as a unicode tree diagram
    View,
    /// Graph overview
    Stats,
    /// Voice commands
    Voice {
        #[command(subcommand)]
        action: Option<VoiceCommand>,
    },
    /// Install or update bundled taxonomer agents into .claude/agents/
    InstallAgents {
        /// Target directory (default: ./.claude/agents/)
        #[arg(long)]
        target: Option<String>,
    },
    /// Health check: binary version, schema version, agent file freshness
    Doctor,
    /// Delete all data from the graph
    Reset,
    /// Export full graph as JSON
    Export,
    /// Import from JSON export
    Import {
        /// JSON file path
        file: String,
    },
}

#[derive(Subcommand)]
enum VoiceCommand {
    /// Update voice content
    Set {
        /// New voice content
        content: String,
    },
    /// Open voice in $EDITOR
    Edit,
}

fn parse_position(s: &str) -> Position {
    match s {
        "start" => Position::Start,
        "end" => Position::End,
        s if s.starts_with("after:") => Position::After(s[6..].to_string()),
        _ => Position::End,
    }
}

fn parse_edge_kind(s: &str) -> std::result::Result<EdgeKind, CairnError> {
    EdgeKind::from_table_name(s).ok_or_else(|| CairnError::InvalidEdgeType(s.to_string()))
}

fn parse_severity(s: &str) -> Severity {
    match s {
        "low" => Severity::Low,
        "high" => Severity::High,
        "critical" => Severity::Critical,
        _ => Severity::Medium,
    }
}

/// Print result as JSON or human-readable text.
macro_rules! output {
    ($json:expr, $val:expr, $fmt:expr) => {
        if $json {
            println!(
                "{}",
                serde_json::to_string_pretty(&$val).unwrap_or_else(|_| "{}".into())
            );
        } else {
            print!("{}", $fmt);
        }
    };
}

/// Write all bundled agent files into the target directory.
fn install_agents_to(target: &std::path::Path) -> std::io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(target)?;
    let mut written = Vec::new();
    for (name, content) in BUNDLED_AGENTS {
        let path = target.join(name);
        std::fs::write(&path, content)?;
        written.push(path);
    }
    Ok(written)
}

#[derive(Debug)]
enum AgentStatus {
    Match,
    Differs,
    Missing,
}

/// Compare each bundled agent against the file in the target directory.
fn check_agents(target: &std::path::Path) -> Vec<(&'static str, AgentStatus)> {
    BUNDLED_AGENTS
        .iter()
        .map(|(name, bundled)| {
            let path = target.join(name);
            let status = if !path.exists() {
                AgentStatus::Missing
            } else {
                match std::fs::read_to_string(&path) {
                    Ok(installed) if installed == *bundled => AgentStatus::Match,
                    Ok(_) => AgentStatus::Differs,
                    Err(_) => AgentStatus::Missing,
                }
            };
            (*name, status)
        })
        .collect()
}

fn render_tree(view: &GraphViewResult) -> String {
    use std::collections::BTreeMap;

    // Group topics by prefix (everything before last '/')
    let mut groups: BTreeMap<String, Vec<&NodeSummary>> = BTreeMap::new();
    for topic in &view.topics {
        let prefix = if let Some(pos) = topic.key.rfind('/') {
            topic.key[..pos].to_string()
        } else {
            String::new()
        };
        groups.entry(prefix).or_default().push(topic);
    }

    // Build edge lookup: topic_key -> list of (direction, edge_type, other_key)
    let mut edge_map: std::collections::HashMap<String, Vec<(char, String, String)>> =
        std::collections::HashMap::new();
    for e in &view.edges {
        edge_map.entry(e.from.clone()).or_default().push((
            '\u{2192}',
            e.edge_type.clone(),
            e.to.clone(),
        )); // →
        edge_map.entry(e.to.clone()).or_default().push((
            '\u{2190}',
            e.edge_type.clone(),
            e.from.clone(),
        )); // ←
    }

    let mut out = String::new();

    for (prefix, topics) in &groups {
        if !prefix.is_empty() {
            out.push_str(&format!("{}/\n", prefix));
        }

        for (i, topic) in topics.iter().enumerate() {
            let is_last_topic = i == topics.len() - 1;
            let branch = if prefix.is_empty() {
                String::new()
            } else if is_last_topic {
                "\u{2514}\u{2500}\u{2500} ".to_string() // └──
            } else {
                "\u{251C}\u{2500}\u{2500} ".to_string() // ├──
            };

            let short_name = if let Some(pos) = topic.key.rfind('/') {
                &topic.key[pos + 1..]
            } else {
                &topic.key
            };

            out.push_str(&format!("{}{} - {}\n", branch, short_name, topic.title));

            // Show edges for this topic
            if let Some(edges) = edge_map.get(&topic.key) {
                // Group edges by (direction, type) -> list of targets
                let mut edge_groups: BTreeMap<(char, &str), Vec<&str>> = BTreeMap::new();
                for (dir, etype, other) in edges {
                    edge_groups
                        .entry((*dir, etype.as_str()))
                        .or_default()
                        .push(other.as_str());
                }

                let edge_list: Vec<_> = edge_groups.into_iter().collect();
                let indent = if prefix.is_empty() {
                    "  "
                } else if is_last_topic {
                    "    "
                } else {
                    "\u{2502}   " // │
                };

                for (j, ((dir, etype), targets)) in edge_list.iter().enumerate() {
                    let is_last_edge = j == edge_list.len() - 1;
                    let edge_branch = if is_last_edge {
                        "\u{2514}\u{2500}\u{2500} " // └──
                    } else {
                        "\u{251C}\u{2500}\u{2500} " // ├──
                    };

                    let prefix_sym = if *etype == "gotcha" || *etype == "war_story" {
                        "\u{26A0} "
                    } else {
                        ""
                    };

                    out.push_str(&format!(
                        "{}{}{}{} {} {}\n",
                        indent,
                        edge_branch,
                        prefix_sym,
                        etype,
                        dir,
                        targets.join(", ")
                    ));
                }
            }
        }
        out.push('\n');
    }

    out
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing_subscriber::filter::LevelFilter::WARN.into()),
        )
        .init();

    let cli = Cli::parse();
    let json = cli.json;

    let db_path = cli.db.map(PathBuf::from).unwrap_or_else(default_db_path);

    // Init is special — it creates the directory structure
    if matches!(cli.command, Command::Init { .. }) {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let cairn = CairnClient::connect_or_spawn(&db_path).await?;

    match cli.command {
        Command::Init { voice, taxonomy } => {
            cairn.init_defaults(voice.as_deref()).await?;

            // Create hooks directory
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            let hooks_dir = PathBuf::from(&home).join(".cairn").join("hooks");
            std::fs::create_dir_all(&hooks_dir)?;
            let logs_dir = PathBuf::from(&home).join(".cairn").join("logs");
            std::fs::create_dir_all(&logs_dir)?;

            if json {
                println!(
                    r#"{{"status": "initialized", "db_path": "{}"}}"#,
                    db_path.display()
                );
            } else {
                println!("Cairn initialized at {}", db_path.display());
                println!("  Hooks dir: {}", hooks_dir.display());
                println!("  Logs dir:  {}", logs_dir.display());
            }

            // Handle taxonomy option
            if let Some(taxonomy) = taxonomy {
                if taxonomy == "scan" {
                    let agent_dir = std::env::current_dir()?.join(".claude").join("agents");
                    install_agents_to(&agent_dir)?;
                    println!(
                        "\nAll taxonomer agents installed at {}",
                        agent_dir.display()
                    );
                    println!("Run the initial scan with: /agents/taxonomer");
                } else {
                    // Treat as "describe Domain1, Domain2, ..."
                    let description = taxonomy.strip_prefix("describe ").unwrap_or(&taxonomy);
                    for domain in description
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        let key = domain.to_lowercase().replace(' ', "-");
                        cairn
                            .learn(LearnParams {
                                topic_key: key.clone(),
                                title: Some(domain.to_string()),
                                summary: Some(format!("Root domain: {domain}")),
                                content: format!("Initial taxonomy entry for {domain}."),
                                voice: None,
                                tags: vec!["taxonomy".into()],
                                position: Position::End,
                                extra_blocks: vec![],
                            })
                            .await?;
                        println!("  Created topic: {key}");
                    }
                }
            }
        }

        Command::Status => {
            let result = cairn.graph_status().await?;
            output!(
                json,
                result,
                format!(
                "Active: {}\nDB: {}\nTopics: {} ({} active, {} deprecated, {} stale)\nVoice: {}\n",
                result.active,
                result.db_path,
                result.stats.total,
                result.stats.active,
                result.stats.deprecated,
                result.stats.stale_90d,
                result.voice.as_deref().map(|v| {
                    if v.len() > 80 { format!("{}...", &v[..80]) } else { v.to_string() }
                }).unwrap_or_else(|| "(none)".into()),
            )
            );
        }

        Command::Prime { task, max_tokens } => {
            let result = cairn.prime(PrimeParams { task, max_tokens }).await?;
            output!(
                json,
                result,
                format!(
                    "{}\n\n---\nMatched: {:?}\nRelated: {:?}\nTokens: ~{}\n",
                    result.context,
                    result.matched_topics,
                    result.related_topics,
                    result.token_estimate
                )
            );
        }

        Command::Learn {
            topic_key,
            content,
            title,
            summary,
            voice,
            tag,
            position,
            ..
        } => {
            let result = cairn
                .learn(LearnParams {
                    topic_key,
                    title,
                    summary,
                    content,
                    voice,
                    tags: tag,
                    position: parse_position(&position),
                    extra_blocks: vec![],
                })
                .await?;
            output!(
                json,
                result,
                format!(
                    "{} topic '{}' (block {}, {} blocks total)\n",
                    result.action, result.topic_key, result.block_id, result.topic_block_count
                )
            );
        }

        Command::Connect {
            from,
            to,
            edge_type,
            note,
            severity,
        } => {
            let kind = parse_edge_kind(&edge_type)?;
            let result = cairn
                .connect_topics(ConnectParams {
                    from_key: from,
                    to_key: to,
                    edge_type: kind,
                    note: note.unwrap_or_default(),
                    severity: severity.map(|s| parse_severity(&s)),
                })
                .await?;
            output!(
                json,
                result,
                format!(
                    "{} {} edge: {} -> {} ({})\n",
                    result.action, result.edge, result.from, result.to, result.note
                )
            );
        }

        Command::Amend {
            topic_key,
            block_id,
            new_content,
            reason,
        } => {
            let result = cairn
                .amend(AmendParams {
                    topic_key,
                    block_id,
                    new_content,
                    reason: reason.unwrap_or_default(),
                })
                .await?;
            output!(
                json,
                result,
                format!(
                    "Amended block {} in '{}': {}\n",
                    result.block_id, result.topic_key, result.reason
                )
            );
        }

        Command::Search {
            query,
            expand,
            limit,
        } => {
            let result = cairn
                .search(SearchParams {
                    query,
                    expand,
                    limit,
                })
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("{} results:\n", result.total_matches);
                for item in &result.results {
                    println!("  {} (score: {:.2})", item.topic_key, item.score);
                    println!("    {}", item.title);
                    if !item.summary.is_empty() {
                        println!("    {}", item.summary);
                    }
                    for n in &item.neighbors {
                        println!("    -> {} [{}] {}", n.key, n.edge, n.title);
                    }
                    println!();
                }
            }
        }

        Command::Explore { topic_key, depth } => {
            let result = cairn
                .explore(ExploreParams {
                    topic_key,
                    depth,
                    edge_types: vec![],
                })
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("Center: {}\n", result.center);
                println!("Nodes:");
                for n in &result.nodes {
                    println!("  {} - {}", n.key, n.title);
                }
                println!("\nEdges:");
                for e in &result.edges {
                    println!("  {} --[{}]--> {} ({})", e.from, e.edge_type, e.to, e.note);
                }
            }
        }

        Command::Path {
            from,
            to,
            max_depth,
        } => {
            let result = cairn
                .path(PathParams {
                    from_key: from,
                    to_key: to,
                    max_depth,
                })
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else if result.found {
                println!("Path found ({} hops):\n", result.depth);
                for step in &result.path {
                    if let Some(node) = &step.node {
                        println!("  [{}]", node);
                    }
                    if let Some(edge) = &step.edge {
                        let note = step.note.as_deref().unwrap_or("");
                        println!("    --{}--> {}", edge, note);
                    }
                }
            } else {
                println!("No path found.");
            }
        }

        Command::Nearby { topic_key, hops } => {
            let result = cairn.nearby(NearbyParams { topic_key, hops }).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("Nearby {} ({} nodes):\n", result.center, result.total_nodes);
                for (edge_type, entries) in &result.by_edge_type {
                    println!("  {}:", edge_type);
                    for e in entries {
                        println!("    {} - {} (distance: {})", e.key, e.title, e.distance);
                    }
                }
            }
        }

        Command::Checkpoint {
            session_id,
            emergency,
        } => {
            let sid = session_id
                .unwrap_or_else(|| chrono::Utc::now().format("sess_%Y%m%d_%H%M%S").to_string());
            let result = cairn
                .checkpoint(CheckpointParams {
                    session_id: sid,
                    emergency,
                })
                .await?;
            output!(
                json,
                result,
                format!(
                    "Checkpoint: session={}, mutations={}, emergency={}\n",
                    result.session_id, result.mutations_persisted, result.emergency
                )
            );
        }

        Command::Snapshot { name } => {
            let result = cairn.snapshot(SnapshotParams { name, path: None }).await?;
            output!(
                json,
                result,
                format!(
                    "Snapshot '{}' saved to {} ({} bytes)\n",
                    result.name, result.path, result.size_bytes
                )
            );
        }

        Command::Restore { name } => {
            let result = cairn.restore(RestoreParams { name }).await?;
            output!(
                json,
                result,
                format!(
                    "Restored from '{}' ({} topics, {} edges)\nSafety snapshot: {}\n",
                    result.restored_from,
                    result.topics_restored,
                    result.edges_restored,
                    result.safety_snapshot
                )
            );
        }

        Command::Forget { topic_key, reason } => {
            let result = cairn
                .forget(ForgetParams {
                    topic_key,
                    reason: reason.unwrap_or_default(),
                })
                .await?;
            output!(
                json,
                result,
                format!("Deprecated '{}': {}\n", result.topic_key, result.reason)
            );
        }

        Command::Rewrite {
            topic_key,
            reason,
            file,
        } => {
            let content = if let Some(path) = file {
                std::fs::read_to_string(path)?
            } else {
                use std::io::Read;
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf)?;
                buf
            };

            let result = cairn
                .rewrite(RewriteParams {
                    topic_key,
                    new_blocks: vec![NewBlock {
                        content,
                        voice: None,
                    }],
                    reason: reason.unwrap_or_default(),
                })
                .await?;
            output!(
                json,
                result,
                format!(
                    "Rewritten '{}': {} -> {} blocks ({})\n",
                    result.topic_key, result.old_block_count, result.new_block_count, result.reason
                )
            );
        }

        Command::History {
            topic_key,
            limit,
            session,
        } => {
            let result = cairn
                .history(HistoryParams {
                    topic_key,
                    limit,
                    session_id: session,
                })
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                for event in &result.events {
                    println!(
                        "  [{}] {} {} - {}",
                        event.created_at.format("%Y-%m-%d %H:%M"),
                        event.op,
                        event.target,
                        event.detail
                    );
                }
            }
        }

        Command::Rename { old_key, new_key } => {
            let result = cairn.rename(RenameParams { old_key, new_key }).await?;
            output!(
                json,
                result,
                format!(
                    "Renamed '{}' -> '{}' ({})\n",
                    result.old_key, result.new_key, result.title
                )
            );
        }

        Command::View => {
            let view = cairn.graph_view().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&view)?);
            } else {
                println!(
                    "Cairn Graph: {} topics, {} edges\n",
                    view.topics.len(),
                    view.edges.len()
                );
                print!("{}", render_tree(&view));
            }
        }

        Command::Stats => {
            let result = cairn.stats().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!(
                    "Topics: {} total, {} active, {} deprecated, {} stale",
                    result.topics.total,
                    result.topics.active,
                    result.topics.deprecated,
                    result.topics.stale_90d
                );
                println!("Edges: {} total", result.edges.total);
                for (t, c) in &result.edges.by_type {
                    if *c > 0 {
                        println!("  {}: {}", t, c);
                    }
                }
                if !result.most_connected.is_empty() {
                    println!("\nMost connected:");
                    for t in &result.most_connected {
                        if t.edge_count.unwrap_or(0) > 0 {
                            println!("  {} ({} edges)", t.key, t.edge_count.unwrap_or(0));
                        }
                    }
                }
            }
        }

        Command::Voice { action } => {
            match action {
                None => {
                    // Read voice
                    let voice = cairn.get_voice().await?;
                    if let Some(v) = voice {
                        if json {
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&VoiceResult {
                                    content: v.content.clone(),
                                    updated_at: v.updated_at,
                                })?
                            );
                        } else {
                            println!("{}", v.content);
                        }
                    } else {
                        println!(
                            "No voice configured. Use `cairn voice set <content>` to set one."
                        );
                    }
                }
                Some(VoiceCommand::Set { content }) => {
                    let result = cairn.set_voice(&content).await?;
                    output!(json, result, format!("Voice updated.\n"));
                }
                Some(VoiceCommand::Edit) => {
                    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
                    let voice = cairn.get_voice().await?;
                    let content = voice.map(|v| v.content).unwrap_or_default();

                    let tmp = std::env::temp_dir().join("cairn_voice.md");
                    std::fs::write(&tmp, &content)?;

                    let status = std::process::Command::new(&editor).arg(&tmp).status()?;

                    if status.success() {
                        let new_content = std::fs::read_to_string(&tmp)?;
                        if new_content != content {
                            cairn.set_voice(&new_content).await?;
                            println!("Voice updated.");
                        } else {
                            println!("No changes.");
                        }
                    }
                    let _ = std::fs::remove_file(&tmp);
                }
            }
        }

        Command::InstallAgents { target } => {
            let target_dir = match target {
                Some(t) => PathBuf::from(t),
                None => std::env::current_dir()?.join(".claude").join("agents"),
            };
            let written = install_agents_to(&target_dir)?;
            if json {
                let paths: Vec<String> = written.iter().map(|p| p.display().to_string()).collect();
                println!("{}", serde_json::to_string_pretty(&paths)?);
            } else {
                println!(
                    "Installed {} agent(s) to {}:",
                    written.len(),
                    target_dir.display()
                );
                for p in &written {
                    if let Some(name) = p.file_name() {
                        println!("  {}", name.to_string_lossy());
                    }
                }
            }
        }

        Command::Doctor => {
            let bin_version = env!("CARGO_PKG_VERSION");
            let bin_schema = CURRENT_SCHEMA_VERSION;
            let db_schema = cairn.schema_version().await?;
            let agent_target = std::env::current_dir()?.join(".claude").join("agents");
            let agent_status = check_agents(&agent_target);

            // Daemon status: we know the daemon is running because we got here
            // (CairnClient::connect_or_spawn succeeded). Show the socket path.
            let socket_path = cairn.socket_path().display().to_string();
            let server_status = "running";

            let schema_status = if db_schema == bin_schema {
                "OK"
            } else if db_schema < bin_schema {
                "older — migrations would be applied on next open"
            } else {
                "NEWER than binary — update cairn-cli/cairn-mcp"
            };

            if json {
                let report = serde_json::json!({
                    "binary_version": bin_version,
                    "binary_schema_version": bin_schema,
                    "db_schema_version": db_schema,
                    "schema_status": schema_status,
                    "server": {
                        "socket": socket_path,
                        "status": server_status,
                    },
                    "agent_target": agent_target.display().to_string(),
                    "agents": agent_status.iter().map(|(name, status)| {
                        serde_json::json!({
                            "name": name,
                            "status": format!("{:?}", status).to_lowercase(),
                        })
                    }).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("Cairn doctor");
                println!();
                println!("Binary:");
                println!("  cairn-cli version: {}", bin_version);
                println!("  schema support:    v{}", bin_schema);
                println!();
                println!("Server:");
                println!("  socket:            {}", socket_path);
                println!("  status:            {}", server_status);
                println!();
                println!("Database ({}):", cairn.db_path());
                println!("  schema version:    v{}", db_schema);
                println!("  status:            {}", schema_status);
                println!();
                println!("Agents in {}:", agent_target.display());
                for (name, status) in &agent_status {
                    let mark = match status {
                        AgentStatus::Match => "✓",
                        AgentStatus::Differs => "✗",
                        AgentStatus::Missing => "·",
                    };
                    let label = match status {
                        AgentStatus::Match => "match",
                        AgentStatus::Differs => {
                            "differs from bundled — run `cairn-cli install-agents`"
                        }
                        AgentStatus::Missing => "missing",
                    };
                    println!("  {} {:<22} {}", mark, name, label);
                }
            }
        }

        Command::Reset => {
            cairn.reset().await?;
            if json {
                println!(r#"{{"status": "reset"}}"#);
            } else {
                println!("All data deleted.");
            }
        }

        Command::Export => {
            let json_out = cairn.export_json().await?;
            println!("{json_out}");
        }

        Command::Import { file } => {
            let data = std::fs::read_to_string(&file)?;
            let (topics, edges) = cairn.import_json(&data).await?;
            if json {
                println!(
                    r#"{{"topics_imported": {}, "edges_imported": {}}}"#,
                    topics, edges
                );
            } else {
                println!("Imported {} topics and {} edges.", topics, edges);
            }
        }
    }

    Ok(())
}
