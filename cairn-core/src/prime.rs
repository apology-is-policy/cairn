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

    // 3. Add matched topics (full content)
    for item in &search_result.results {
        if token_count >= max_tokens {
            break;
        }

        matched_topics.push(item.topic_key.clone());

        // Fetch full topic for blocks
        let topic = crate::ops::get_topic_by_key(db, &item.topic_key).await?;

        let mut section = format!("## {}\n\n", item.title);
        if !item.summary.is_empty() {
            section.push_str(&item.summary);
            section.push('\n');
        }

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

    // 6. Situational notes based on what was (or wasn't) matched.
    let mut notes: Vec<String> = Vec::new();

    if matched_topics.is_empty() && !keywords.is_empty() {
        notes.push(
            "No existing topics matched your task. This is likely a new area — \
             create a topic for it as you work so future sessions have context."
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
                stale_keys.push(format!(
                    "{} ({}d old)",
                    key,
                    age.num_days()
                ));
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
        let notes_section = format!(
            "\n## ⚠ Notes for this task\n\n{}\n",
            notes.join("\n\n")
        );
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
            },
        )
        .await
        .unwrap();

        let result = graph_status(&db).await.unwrap();
        assert!(result.active);
        assert_eq!(result.stats.total, 1);
        assert_eq!(result.voice.as_deref(), Some("I write Rust."));
    }
}
