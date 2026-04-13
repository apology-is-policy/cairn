use std::collections::HashSet;

use chrono::Utc;
use serde::Deserialize;

use crate::db::CairnDb;
use crate::error::{CairnError, Result};
use crate::protocol::generate_protocol;
use crate::search;
use crate::types::*;

// ── Voice & Preferences CRUD ─────────────────────────────────────

/// Read the developer's voice node.
pub async fn get_voice(db: &CairnDb) -> Result<Option<Voice>> {
    #[derive(Deserialize)]
    struct VoiceRow {
        content: String,
        updated_at: chrono::DateTime<Utc>,
    }

    let mut res = db
        .db
        .query("SELECT content, updated_at FROM voice LIMIT 1")
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;

    let row: Option<VoiceRow> = res.take(0).map_err(|e| CairnError::Db(e.to_string()))?;
    Ok(row.map(|r| Voice {
        content: r.content,
        updated_at: r.updated_at,
    }))
}

/// Update the developer's voice node.
pub async fn set_voice(db: &CairnDb, content: &str) -> Result<VoiceResult> {
    // Check if voice exists
    let existing = get_voice(db).await?;

    if existing.is_some() {
        db.db
            .query("UPDATE voice SET content = $content, updated_at = time::now()")
            .bind(("content", content.to_string()))
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;
    } else {
        db.db
            .query("CREATE voice SET content = $content, updated_at = time::now()")
            .bind(("content", content.to_string()))
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;
    }

    Ok(VoiceResult {
        content: content.to_string(),
        updated_at: Utc::now(),
    })
}

/// Read the preferences node.
pub async fn get_preferences(db: &CairnDb) -> Result<Preferences> {
    #[derive(Deserialize)]
    struct PrefsRow {
        prime_max_tokens: i64,
        prime_include_gotchas: bool,
        learn_verbosity: String,
        learn_auto: bool,
        updated_at: chrono::DateTime<Utc>,
    }

    let mut res = db
        .db
        .query("SELECT prime_max_tokens, prime_include_gotchas, learn_verbosity, learn_auto, updated_at FROM preferences LIMIT 1")
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;

    let row: Option<PrefsRow> = res.take(0).map_err(|e| CairnError::Db(e.to_string()))?;
    Ok(row
        .map(|r| Preferences {
            prime_max_tokens: r.prime_max_tokens,
            prime_include_gotchas: r.prime_include_gotchas,
            learn_verbosity: r.learn_verbosity,
            learn_auto: r.learn_auto,
            updated_at: r.updated_at,
        })
        .unwrap_or_default())
}

/// Update the preferences node.
pub async fn set_preferences(db: &CairnDb, prefs: &Preferences) -> Result<()> {
    let existing = get_preferences(db).await?;
    let _ = existing; // just to check it exists

    // Upsert: if preferences exist, update; otherwise create
    db.db
        .query(
            "DELETE preferences;
            CREATE preferences SET
                prime_max_tokens = $pmt,
                prime_include_gotchas = $pig,
                learn_verbosity = $lv,
                learn_auto = $la,
                updated_at = time::now()",
        )
        .bind(("pmt", prefs.prime_max_tokens))
        .bind(("pig", prefs.prime_include_gotchas))
        .bind(("lv", prefs.learn_verbosity.clone()))
        .bind(("la", prefs.learn_auto))
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;

    Ok(())
}

/// Initialize default voice and preferences if they don't exist.
pub async fn init_defaults(db: &CairnDb, initial_voice: Option<&str>) -> Result<()> {
    let voice = get_voice(db).await?;
    if voice.is_none() {
        let content = initial_voice.unwrap_or(
            "No voice configured yet. Use `voice set` to describe your coding style, opinions, and preferences.",
        );
        set_voice(db, content).await?;
    }

    // Check if preferences exist
    #[derive(Deserialize)]
    struct CountRow {
        count: usize,
    }
    let mut res = db
        .db
        .query("SELECT count() AS count FROM preferences GROUP ALL")
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;
    let count: usize = res
        .take::<Option<CountRow>>(0)
        .map_err(|e| CairnError::Db(e.to_string()))?
        .map(|r| r.count)
        .unwrap_or(0);

    if count == 0 {
        set_preferences(db, &Preferences::default()).await?;
    }

    Ok(())
}

// ── Prime (context composition) ──────────────────────────────────

/// Simple stop words to filter from task descriptions before FTS search.
const STOP_WORDS: &[&str] = &[
    "a", "an", "the", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
    "do", "does", "did", "will", "would", "could", "should", "may", "might", "can", "shall", "to",
    "of", "in", "for", "on", "with", "at", "by", "from", "as", "into", "through", "during",
    "before", "after", "above", "below", "between", "and", "but", "or", "not", "no", "nor", "this",
    "that", "these", "those", "it", "its", "i", "me", "my", "we", "our", "you", "your", "he",
    "she", "they", "them", "their", "what", "which", "who", "when", "where", "why", "how", "all",
    "each", "every", "both", "few", "more", "most", "other", "some", "such", "only",
];

fn extract_keywords(task: &str) -> Vec<String> {
    task.split_whitespace()
        .map(|w| {
            w.to_lowercase()
                .replace(|c: char| !c.is_alphanumeric() && c != '-', "")
        })
        .filter(|w| w.len() > 2 && !STOP_WORDS.contains(&w.as_str()))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect()
}

/// Estimate token count using chars/4 heuristic.
fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

/// Compose and return relevant context for a task.
/// Build a pre-flight briefing from the graph structure around matched topics.
///
/// Traverses 2-hop edges to find:
/// - **Constraints**: gotcha edges with severity and notes
/// - **Impact radius**: reverse depends_on (what depends on your area)
/// - **Contradictions**: contradicts edges between or near matched topics
/// - **War stories**: war_story edges within 2 hops
/// - **Stale areas**: matched topics not updated in 30+ days
///
/// Returns formatted markdown sections, or empty string if nothing notable.
async fn build_preflight(db: &CairnDb, matched_keys: &[String], _task: &str) -> Result<String> {
    use std::fmt::Write;

    // Get all edges within 2 hops of matched topics.
    let hop1_edges = search::edges_for_matched(db, matched_keys).await?;

    // Collect neighbor keys for 2nd hop.
    let mut hop1_keys: HashSet<String> = HashSet::new();
    for e in &hop1_edges {
        hop1_keys.insert(e.from.clone());
        hop1_keys.insert(e.to.clone());
    }
    let hop2_keys: Vec<String> = hop1_keys
        .iter()
        .filter(|k| !matched_keys.contains(k))
        .cloned()
        .collect();
    let hop2_edges = if !hop2_keys.is_empty() {
        search::edges_for_matched(db, &hop2_keys).await?
    } else {
        vec![]
    };

    let matched_set: HashSet<&str> = matched_keys.iter().map(|s| s.as_str()).collect();
    let mut sections = String::new();

    // ── Constraints (gotchas within 2 hops) ──
    let mut gotchas: Vec<String> = Vec::new();
    for e in hop1_edges.iter().chain(hop2_edges.iter()) {
        if e.edge_type == "gotcha" {
            let involves_matched =
                matched_set.contains(e.from.as_str()) || matched_set.contains(e.to.as_str());
            if involves_matched {
                gotchas.push(format!("- {} → {} (gotcha): {}", e.from, e.to, e.note));
            }
        }
    }
    if !gotchas.is_empty() {
        let _ = writeln!(
            sections,
            "### Constraints (gotchas)\n{}\n",
            gotchas.join("\n")
        );
    }

    // ── Impact radius (reverse depends_on) ──
    let mut dependents: Vec<String> = Vec::new();
    for e in &hop1_edges {
        if e.edge_type == "depends_on" && matched_set.contains(e.to.as_str()) {
            dependents.push(format!("- {} depends on {} — {}", e.from, e.to, e.note));
        }
    }
    // Transitive dependents (2nd hop)
    for e in &hop2_edges {
        if e.edge_type == "depends_on" {
            // Check if to_key is a hop-1 dependent
            let is_transitive = hop1_edges.iter().any(|h1| {
                h1.edge_type == "depends_on"
                    && h1.from == e.to
                    && matched_set.contains(h1.to.as_str())
            });
            if is_transitive {
                dependents.push(format!(
                    "- {} depends on {} (transitive) — {}",
                    e.from, e.to, e.note
                ));
            }
        }
    }
    if !dependents.is_empty() {
        let _ = writeln!(
            sections,
            "### Impact radius (what depends on your area)\n{}\n",
            dependents.join("\n")
        );
    }

    // ── War stories ──
    let mut war_stories: Vec<String> = Vec::new();
    for e in hop1_edges.iter().chain(hop2_edges.iter()) {
        if e.edge_type == "war_story" {
            let involves =
                matched_set.contains(e.from.as_str()) || matched_set.contains(e.to.as_str());
            if involves {
                war_stories.push(format!("- {} ↔ {} (war story): {}", e.from, e.to, e.note));
            }
        }
    }
    if !war_stories.is_empty() {
        let _ = writeln!(sections, "### War stories\n{}\n", war_stories.join("\n"));
    }

    // ── Contradictions ──
    let mut contradictions: Vec<String> = Vec::new();
    for e in &hop1_edges {
        if e.edge_type == "contradicts" {
            contradictions.push(format!("- {} contradicts {} — {}", e.from, e.to, e.note));
        }
    }
    if !contradictions.is_empty() {
        let _ = writeln!(
            sections,
            "### Contradictions (verify which is current)\n{}\n",
            contradictions.join("\n")
        );
    }

    // ── Stale areas ──
    let now = chrono::Utc::now();
    let mut stale: Vec<String> = Vec::new();
    for key in matched_keys {
        if let Some(topic) = crate::ops::get_topic_by_key(db, key).await? {
            let age_days = now.signed_duration_since(topic.updated_at).num_days();
            if age_days > 30 {
                stale.push(format!(
                    "- {} ({}d since last update) — verify before relying on it",
                    key, age_days
                ));
            }
        }
    }
    if !stale.is_empty() {
        let _ = writeln!(sections, "### Stale areas\n{}\n", stale.join("\n"));
    }

    Ok(sections)
}

pub async fn prime(db: &CairnDb, params: PrimeParams) -> Result<PrimeResult> {
    let prefs = get_preferences(db).await?;
    let max_tokens = params.max_tokens.unwrap_or(prefs.prime_max_tokens) as usize;

    let mut context_parts: Vec<String> = Vec::new();
    let mut token_count = 0usize;
    let mut matched_topics: Vec<String> = Vec::new();
    let mut related_topics: Vec<String> = Vec::new();

    // 1. Voice (always first)
    if let Some(voice) = get_voice(db).await? {
        let voice_section = format!("## Developer Voice\n\n{}\n", voice.content);
        token_count += estimate_tokens(&voice_section);
        context_parts.push(voice_section);
    }

    // 2. Extract keywords and search
    let keywords = extract_keywords(&params.task);
    if keywords.is_empty() {
        // No meaningful keywords — return just voice
        let context = context_parts.join("\n");
        return Ok(PrimeResult {
            context: context.clone(),
            matched_topics,
            related_topics,
            token_estimate: estimate_tokens(&context),
        });
    }

    // Search for each keyword individually and merge results
    let mut seen_keys: HashSet<String> = HashSet::new();
    let mut all_search_items: Vec<SearchResultItem> = Vec::new();

    for keyword in &keywords {
        let result = search::search(
            db,
            SearchParams {
                query: keyword.clone(),
                expand: true,
                limit: 10,
            },
        )
        .await?;

        for item in result.results {
            if seen_keys.insert(item.topic_key.clone()) {
                all_search_items.push(item);
            }
        }
    }

    // Sort by score descending
    all_search_items.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all_search_items.truncate(10);

    let search_result = SearchResult {
        total_matches: all_search_items.len(),
        results: all_search_items,
    };

    // 3. Add matched topics — tier-weighted.
    //    Atlas: full blocks. Journal: summary only. Notes: excluded from prime.
    for item in &search_result.results {
        if token_count >= max_tokens {
            break;
        }

        // Fetch full topic to check tier and locked status.
        let topic = crate::ops::get_topic_by_key(db, &item.topic_key).await?;
        let tier = topic.as_ref().map(|t| t.tier).unwrap_or(TopicTier::Atlas);

        // Notes tier: excluded from prime entirely.
        if tier == TopicTier::Notes {
            continue;
        }

        matched_topics.push(item.topic_key.clone());

        let tier_label = if tier != TopicTier::Atlas {
            format!(" [{}]", tier.label())
        } else {
            String::new()
        };
        let mut section = format!("## {}{}\n\n", item.title, tier_label);

        // Flag locked topics so the agent knows not to modify them.
        if let Some(ref t) = topic {
            if t.locked {
                section.push_str("🔒 **LOCKED** — This topic has been curated by the user. Do not modify, amend, rewrite, or append to it. Treat its content as authoritative.\n\n");
            }
        }

        if !item.summary.is_empty() {
            section.push_str(&item.summary);
            section.push('\n');
        }

        // Journal tier: summary only, no blocks (saves tokens for atlas content).
        if tier == TopicTier::Journal {
            token_count += estimate_tokens(&section);
            context_parts.push(section);
            continue;
        }

        // Atlas tier: include full blocks.
        if let Some(topic) = &topic {
            for block in &topic.blocks {
                let block_text = if let Some(voice) = &block.voice {
                    format!("\n[{}] {}\n", voice, block.content)
                } else {
                    format!("\n{}\n", block.content)
                };

                if token_count + estimate_tokens(&block_text) > max_tokens {
                    break;
                }
                section.push_str(&block_text);
                token_count += estimate_tokens(&block_text);
            }
        }

        token_count += estimate_tokens(&section);
        context_parts.push(section);

        // 4. Add gotchas for this topic (if enabled)
        if prefs.prime_include_gotchas {
            for neighbor in &item.neighbors {
                if neighbor.edge == "gotcha" && token_count < max_tokens {
                    let gotcha_text =
                        format!("\n⚠️ GOTCHA ({}): {}\n", neighbor.key, neighbor.title);
                    token_count += estimate_tokens(&gotcha_text);
                    context_parts.push(gotcha_text);
                }
            }
        }
    }

    // 5. Add related topics (summary only, to save tokens)
    let matched_set: HashSet<&str> = matched_topics.iter().map(|s| s.as_str()).collect();
    for item in &search_result.results {
        for neighbor in &item.neighbors {
            if matched_set.contains(neighbor.key.as_str()) {
                continue;
            }
            if related_topics.contains(&neighbor.key) {
                continue;
            }
            if token_count >= max_tokens {
                break;
            }

            related_topics.push(neighbor.key.clone());
            let related_text = format!(
                "\n### Related: {} (via {})\n{}\n",
                neighbor.title, neighbor.edge, neighbor.key
            );
            token_count += estimate_tokens(&related_text);
            context_parts.push(related_text);
        }
    }

    // 6. Pre-flight briefing — synthesize graph-structural guidance
    //    from edges (gotchas, dependencies, contradictions, war stories)
    //    within 2 hops of matched topics. This turns the graph topology
    //    into active warnings the agent reads before starting work.
    if !matched_topics.is_empty() && token_count < max_tokens {
        let preflight = build_preflight(db, &matched_topics, &params.task).await?;
        if !preflight.is_empty() {
            let section = format!("## Pre-flight for: \"{}\"\n\n{}\n", params.task, preflight);
            let section_tokens = estimate_tokens(&section);
            if token_count + section_tokens <= max_tokens {
                // Insert BEFORE topic content — it's guidance, not reference.
                // Find the position after the voice section.
                let insert_pos = if context_parts.is_empty() {
                    0
                } else {
                    1 // After voice
                };
                context_parts.insert(insert_pos, section);
                token_count += section_tokens;
            }
        }
    }

    // 7. Situational notes based on what was (or wasn't) matched.
    let mut notes: Vec<String> = Vec::new();

    if matched_topics.is_empty() && !keywords.is_empty() {
        notes.push(
            "DISCOVERY AREA: No existing topics matched your task. The graph has \
             no knowledge of this territory. Before completing this task, ask the \
             user: \"I discovered knowledge about [area] that isn't in the graph. \
             Should I catalogue it for future sessions?\" If they say yes, `learn` \
             what you found. If they decline, move on — not every investigation \
             needs to be persisted."
                .into(),
        );
    }

    // Check for stale matched topics.
    let mut stale_keys: Vec<String> = Vec::new();
    let now = chrono::Utc::now();
    for key in &matched_topics {
        if let Some(topic) = crate::ops::get_topic_by_key(db, key).await? {
            let age = now.signed_duration_since(topic.updated_at);
            if age.num_days() > 30 {
                stale_keys.push(format!("{} ({}d old)", key, age.num_days()));
            }
        }
    }
    if !stale_keys.is_empty() {
        notes.push(format!(
            "Stale topics in your context (>30 days since last update): {}. \
             Verify these against the current code before relying on them, \
             and amend any blocks that have drifted.",
            stale_keys.join(", ")
        ));
    }

    if !notes.is_empty() && token_count < max_tokens {
        let notes_section = format!("\n## ⚠ Notes for this task\n\n{}\n", notes.join("\n\n"));
        let _ = estimate_tokens(&notes_section); // token_count not read after this
        context_parts.push(notes_section);
    }

    let context = context_parts.join("\n");
    let token_estimate = estimate_tokens(&context);

    Ok(PrimeResult {
        context,
        matched_topics,
        related_topics,
        token_estimate,
    })
}

/// Return graph status including stats, protocol, and voice.
pub async fn graph_status(db: &CairnDb) -> Result<GraphStatusResult> {
    let stats = search::stats(db).await?;
    let prefs = get_preferences(db).await?;
    let voice = get_voice(db).await?;
    let protocol = generate_protocol(&prefs, &stats.topics, voice.is_some());

    let active = stats.topics.total > 0 || voice.is_some();

    Ok(GraphStatusResult {
        active,
        db_path: db.db_path.clone(),
        stats: stats.topics,
        protocol,
        voice: voice.map(|v| v.content),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops;

    async fn test_db() -> CairnDb {
        CairnDb::open_memory().await.unwrap()
    }

    #[tokio::test]
    async fn test_voice_crud() {
        let db = test_db().await;

        // No voice initially
        assert!(get_voice(&db).await.unwrap().is_none());

        // Set voice
        let result = set_voice(&db, "I write Rust and hate magic.")
            .await
            .unwrap();
        assert_eq!(result.content, "I write Rust and hate magic.");

        // Read back
        let voice = get_voice(&db).await.unwrap().unwrap();
        assert_eq!(voice.content, "I write Rust and hate magic.");

        // Update
        set_voice(&db, "Updated voice.").await.unwrap();
        let voice = get_voice(&db).await.unwrap().unwrap();
        assert_eq!(voice.content, "Updated voice.");
    }

    #[tokio::test]
    async fn test_preferences_crud() {
        let db = test_db().await;

        // Defaults
        let prefs = get_preferences(&db).await.unwrap();
        assert_eq!(prefs.prime_max_tokens, 4000);
        assert!(prefs.learn_auto);

        // Update
        let mut new_prefs = prefs;
        new_prefs.prime_max_tokens = 8000;
        new_prefs.learn_auto = false;
        set_preferences(&db, &new_prefs).await.unwrap();

        let prefs = get_preferences(&db).await.unwrap();
        assert_eq!(prefs.prime_max_tokens, 8000);
        assert!(!prefs.learn_auto);
    }

    #[tokio::test]
    async fn test_init_defaults() {
        let db = test_db().await;

        init_defaults(&db, Some("I love Rust.")).await.unwrap();

        let voice = get_voice(&db).await.unwrap().unwrap();
        assert_eq!(voice.content, "I love Rust.");

        let prefs = get_preferences(&db).await.unwrap();
        assert_eq!(prefs.prime_max_tokens, 4000);

        // Calling again should not overwrite
        init_defaults(&db, Some("Different voice.")).await.unwrap();
        let voice = get_voice(&db).await.unwrap().unwrap();
        assert_eq!(voice.content, "I love Rust.");
    }

    #[tokio::test]
    async fn test_extract_keywords() {
        let keywords = extract_keywords("Fix the billing retry bug in the payment module");
        assert!(keywords.contains(&"billing".to_string()));
        assert!(keywords.contains(&"retry".to_string()));
        assert!(keywords.contains(&"payment".to_string()));
        assert!(!keywords.contains(&"the".to_string()));
        assert!(!keywords.contains(&"in".to_string()));
    }

    #[tokio::test]
    async fn test_prime_empty_graph() {
        let db = test_db().await;
        init_defaults(&db, Some("I write Rust.")).await.unwrap();

        let result = prime(
            &db,
            PrimeParams {
                task: "Fix the billing retry bug".into(),
                max_tokens: None,
            },
        )
        .await
        .unwrap();

        // Should contain voice but no topics
        assert!(result.context.contains("I write Rust."));
        assert!(result.matched_topics.is_empty());
    }

    #[tokio::test]
    async fn test_prime_with_topics() {
        let db = test_db().await;
        init_defaults(&db, Some("I write Rust.")).await.unwrap();

        // Create a topic with matching content
        ops::learn(
            &db,
            LearnParams {
                topic_key: "billing-retry".into(),
                title: Some("Payment retry mechanism".into()),
                summary: Some("Handles payment retry with backoff".into()),
                content: "The retry logic uses exponential backoff with jitter.".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
                tier: None,
            },
        )
        .await
        .unwrap();

        let result = prime(
            &db,
            PrimeParams {
                task: "Fix the billing retry bug".into(),
                max_tokens: None,
            },
        )
        .await
        .unwrap();

        assert!(result.context.contains("I write Rust."));
        assert!(!result.matched_topics.is_empty());
        assert!(result.token_estimate > 0);
    }

    #[tokio::test]
    async fn test_graph_status_empty() {
        let db = test_db().await;

        let result = graph_status(&db).await.unwrap();
        assert!(!result.active);
        assert!(result.protocol.contains("ALWAYS:"));
    }

    #[tokio::test]
    async fn test_graph_status_active() {
        let db = test_db().await;
        init_defaults(&db, Some("I write Rust.")).await.unwrap();

        ops::learn(
            &db,
            LearnParams {
                topic_key: "test".into(),
                title: Some("Test topic".into()),
                summary: None,
                content: "content".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
                tier: None,
            },
        )
        .await
        .unwrap();

        let result = graph_status(&db).await.unwrap();
        assert!(result.active);
        assert_eq!(result.stats.total, 1);
        assert_eq!(result.voice.as_deref(), Some("I write Rust."));
    }

    // ── Pre-flight briefing tests ────────────────────────────────

    /// Helper: create a topic with the given key and title.
    async fn make_topic(db: &CairnDb, key: &str, title: &str, content: &str) {
        ops::learn(
            db,
            LearnParams {
                topic_key: key.into(),
                title: Some(title.into()),
                summary: Some(format!("{title} summary")),
                content: content.into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
                tier: None,
            },
        )
        .await
        .unwrap();
    }

    /// Helper: create an edge.
    async fn make_edge(db: &CairnDb, from: &str, to: &str, kind: EdgeKind, note: &str) {
        ops::connect(
            db,
            ConnectParams {
                from_key: from.into(),
                to_key: to.into(),
                edge_type: kind,
                note: note.into(),
                severity: None,
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_preflight_gotcha_constraint() {
        let db = test_db().await;
        init_defaults(&db, Some("voice")).await.unwrap();

        make_topic(&db, "payments/retry", "Payment retry", "Retry logic").await;
        make_topic(&db, "payments/idempotency", "Idempotency", "Dedup logic").await;
        make_edge(
            &db,
            "payments/retry",
            "payments/idempotency",
            EdgeKind::Gotcha,
            "Must check idempotency key before retrying",
        )
        .await;

        let result = prime(
            &db,
            PrimeParams {
                task: "Fix the payment retry timeout".into(),
                max_tokens: None,
            },
        )
        .await
        .unwrap();

        assert!(
            result.context.contains("Constraints (gotchas)"),
            "Pre-flight should include gotcha constraints, got:\n{}",
            result.context
        );
        assert!(result.context.contains("idempotency key"));
    }

    #[tokio::test]
    async fn test_preflight_impact_radius() {
        let db = test_db().await;
        init_defaults(&db, Some("voice")).await.unwrap();

        make_topic(&db, "infra/event-bus", "Event bus", "Message bus").await;
        make_topic(&db, "payments/retry", "Payment retry", "Retry logic").await;
        make_edge(
            &db,
            "payments/retry",
            "infra/event-bus",
            EdgeKind::DependsOn,
            "Retry reads the event bus format header",
        )
        .await;

        let result = prime(
            &db,
            PrimeParams {
                task: "Refactor the event bus serialization".into(),
                max_tokens: None,
            },
        )
        .await
        .unwrap();

        assert!(
            result.context.contains("Impact radius"),
            "Pre-flight should show dependents, got:\n{}",
            result.context
        );
        assert!(result.context.contains("payments/retry depends on"));
    }

    #[tokio::test]
    async fn test_preflight_war_story() {
        let db = test_db().await;
        init_defaults(&db, Some("voice")).await.unwrap();

        make_topic(&db, "payments/webhooks", "Webhooks", "Webhook handler").await;
        make_topic(&db, "incidents/webhook-storm", "Webhook storm", "50k dupes").await;
        make_edge(
            &db,
            "payments/webhooks",
            "incidents/webhook-storm",
            EdgeKind::WarStory,
            "Stripe sent 50k duplicate webhooks, 2h incident",
        )
        .await;

        let result = prime(
            &db,
            PrimeParams {
                task: "Add new webhook endpoint for payment provider".into(),
                max_tokens: None,
            },
        )
        .await
        .unwrap();

        assert!(
            result.context.contains("War stories"),
            "Pre-flight should include war stories, got:\n{}",
            result.context
        );
        assert!(result.context.contains("50k duplicate"));
    }

    #[tokio::test]
    async fn test_preflight_contradiction() {
        let db = test_db().await;
        init_defaults(&db, Some("voice")).await.unwrap();

        make_topic(
            &db,
            "api/rest-spec",
            "REST API specification",
            "All APIs use REST endpoints",
        )
        .await;
        make_topic(
            &db,
            "api/grpc-spec",
            "gRPC API specification",
            "Some APIs use gRPC endpoints",
        )
        .await;
        make_edge(
            &db,
            "api/rest-spec",
            "api/grpc-spec",
            EdgeKind::Contradicts,
            "REST spec says all APIs are REST, but gRPC spec exists",
        )
        .await;

        let result = prime(
            &db,
            PrimeParams {
                task: "Update the REST API spec".into(),
                max_tokens: None,
            },
        )
        .await
        .unwrap();

        assert!(
            result.context.contains("Contradictions"),
            "Pre-flight should flag contradictions, got:\n{}",
            result.context
        );
    }

    #[tokio::test]
    async fn test_preflight_empty_when_no_edges() {
        let db = test_db().await;
        init_defaults(&db, Some("voice")).await.unwrap();

        // Topic with no edges — pre-flight should be empty (no sections).
        make_topic(&db, "standalone", "Standalone module", "No dependencies").await;

        let result = prime(
            &db,
            PrimeParams {
                task: "Work on standalone module".into(),
                max_tokens: None,
            },
        )
        .await
        .unwrap();

        assert!(
            !result.context.contains("Pre-flight"),
            "Pre-flight should be empty for a topic with no edges, got:\n{}",
            result.context
        );
    }

    #[tokio::test]
    async fn test_preflight_appears_before_topic_content() {
        let db = test_db().await;
        init_defaults(&db, Some("voice")).await.unwrap();

        make_topic(&db, "alpha", "Alpha", "Alpha content here").await;
        make_topic(&db, "beta", "Beta", "Beta content here").await;
        make_edge(&db, "alpha", "beta", EdgeKind::Gotcha, "Watch out for beta").await;

        let result = prime(
            &db,
            PrimeParams {
                task: "Work on alpha".into(),
                max_tokens: None,
            },
        )
        .await
        .unwrap();

        let preflight_pos = result.context.find("Pre-flight");
        let content_pos = result.context.find("Alpha content here");
        assert!(
            preflight_pos.is_some() && content_pos.is_some(),
            "Both pre-flight and content should be present"
        );
        assert!(
            preflight_pos.unwrap() < content_pos.unwrap(),
            "Pre-flight should appear BEFORE topic content"
        );
    }
}
