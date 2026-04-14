use crate::types::{Preferences, TopicStats};

const DEFAULT_PROTOCOL: &str = r#"You have an active Cairn knowledge graph for this workspace.

ALWAYS:
- Call `prime` at the start of every task, passing the task description.
- Call `search` before making architectural recommendations, to check for
  prior context or decisions stored in the graph.
- When `prime` returns no matches for your task, you are entering a
  DISCOVERY AREA — the graph has no knowledge of this territory. Before
  completing the task, ask the user whether the knowledge you discovered
  should be catalogued. Not every investigation needs to be persisted, but
  the user should make that call — don't silently skip it.

CATALOGUE THE CODEBASE:
As you work through code, create and maintain topics that describe the logical
structure of the codebase. The quality bar: could someone learn this area from
the graph entry alone, without reading the code first? Aim for that.
- Each significant module, feature, service, or logical chunk gets its own topic.
- Describe WHAT it does, HOW it actually works (the mental model, not a code
  walkthrough), WHERE the key entry points and config live, and WHY it's built
  the way it is — the constraints, tradeoffs, and history behind the design.
- Capture the conceptual architecture: how data flows through the system, what
  the key abstractions are and what they hide, which parts are load-bearing and
  which are incidental, what the domain model actually means.
- Use `connect` to link related areas — dependencies between services, shared
  abstractions, data flows, ownership boundaries.
- Update topics when you see the code has changed from what's recorded.
- Use hierarchical topic keys to reflect structure: "payments/retry",
  "payments/webhooks", "auth/oauth", "auth/sessions".

CATALOGUE WHAT YOU CREATE:
When you create or substantially modify a module, operation, or behavior,
catalogue it immediately — don't defer to a future session.
- HARD RULE: Before creating a git commit, STOP and ask yourself: "Did I
  discover anything the graph doesn't have?" If yes, `learn` it NOW — not
  after the commit, not in a future session, NOW. This is not optional.
  A 30-second `learn` call now saves a future agent 10 minutes of
  re-discovery.
- Prioritize decisions and WHY over structure and WHAT. The code shows what
  exists; the graph should explain why it exists that way and what the
  alternatives were.
- When you introduce a new operation, RPC variant, or core abstraction, that
  is a topic or a block in an existing topic — not something to "get to later."
- If you're unsure whether something deserves a topic, err on the side of
  recording it. A small entry that exists beats a detailed entry that doesn't.

RECORD INSIGHTS AND DISCOVERIES:
- A hidden dependency between modules → `connect` with `depends_on`
- A surprising behavior or bug → `learn` under the relevant topic
- A known pitfall → `connect` with `gotcha` edge
- A reason WHY something is built a certain way → `learn` it
- A past incident relevant to current work → `connect` with `war_story`
- That existing knowledge is wrong → `amend` the specific block

KEEP THE GRAPH FRESH:
The graph is a cache of what was true when topics were last touched. Code
moves faster than the graph, so treat existing topics as a starting hypothesis
and verify before relying on them.
- When you start working substantively in an area covered by an existing
  topic, check the topic's freshness. Use `history <topic-key>` to see when
  the topic was last touched.
- If the topic was last updated more than ~30 days ago AND you'll be making
  non-trivial changes there, do a quick freshness check before trusting it:
  if the project uses git, run `git log --since="<topic.updated_at>" --
  <relevant paths>` and skim the diffs for anything that contradicts the
  topic. Significant refactors, renamed entry points, changed dependencies,
  or removed gotchas all warrant an update.
- When you *create or substantially modify* an area, create or update its
  topic immediately. Don't wait for a future session to catalogue it.
- When you find the code has diverged from what cairn says, ALWAYS `amend`
  the specific stale block rather than appending a new one. This preserves
  the audit trail (old content saved to history) and prevents topics from
  disagreeing with themselves.
- If the gotcha no longer applies, amend it or remove the edge with a note
  explaining why. Stale gotchas waste future agents' attention.
- This freshness check is for substantive work — making changes, recommending
  architecture, debugging in the area. Don't do it for skim-reads or quick
  reference lookups; the cost isn't worth it for one-off questions.

PERIODIC MAINTENANCE:
In long sessions with many mutations (10+ learn/connect/amend calls), pause
periodically to check whether the areas you're working in have up-to-date
topics. If you've built something new and haven't catalogued it, do so before
the context window compresses and you lose the understanding you have now.
A good rhythm: after every major commit or logical milestone, take 30 seconds
to ensure the graph reflects what just happened.

SEARCH BEFORE RECOMMENDING:
Before making architectural recommendations or proposing design changes, call
`search` to check whether the graph contains prior context, decisions, or
war stories about the area. This applies whether you're advising on existing
code OR creating new architecture — the graph may contain constraints or
decisions from prior sessions that should inform your current approach. If
search returns nothing relevant, proceed with your recommendation but note
the gap.

TASK COMPLETION CHECKLIST:
Before reporting a task as done to the user, run this 10-second self-check:
1. Did I discover how something works that isn't in the graph? → `learn` it.
2. Did I find a relationship between areas? → `connect` them.
3. Did I find something wrong or outdated in the graph? → `amend` it.
4. Did `prime` return nothing at the start? → I'm in a discovery area.
   Ask the user if the knowledge should be catalogued before closing.
If all four are "no," you're done. If any is "yes," record it before
responding. This check is mandatory, not aspirational.

TOPIC TIERS:
Topics have a tier that controls their weight in prime and search:
- **atlas** (default) — authoritative codebase knowledge. Describes what
  a module IS, how it works, why it's built that way. Full blocks included
  in prime. Use for: module descriptions, architecture, API contracts,
  configuration guides, gotchas.
- **journal** — task records, decisions, and outcomes. Describes what
  HAPPENED. Summary-only in prime (saves tokens for atlas). Use for: task
  plans, PR context, rollout decisions, incident post-mortems, temporary
  changes with intent to revert.
- **notes** — scratch, hypotheses, investigation logs. Excluded from prime
  entirely, only appears in explicit search. Use for: "I think X might be
  causing Y", debugging notes, questions to investigate later.

When creating a topic with `learn`, pass `tier: "journal"` or `tier: "notes"`
to set it. Default is atlas. When recording task context (plans, decisions,
what you did and why), use journal. When jotting a quick hypothesis or
scratch note, use notes. When cataloguing codebase structure, use atlas.

BLOCK GRANULARITY:
- A block should be one coherent idea — typically 1-5 paragraphs.
- If a block exceeds ~500 words, consider splitting into multiple blocks with
  distinct concerns.
- If a topic has more than 10 blocks, consider splitting into sub-topics.
- When creating a new topic with rich content, use `learn` with `extra_blocks`
  to create a well-structured multi-block topic in a single call, rather than
  making multiple `learn` calls to append blocks one by one.
- When rewriting multiple topics at once (e.g., during a research or cataloguing
  session), use `batch_rewrite` to process them all in a single RPC call.

DO NOT LOG:
- Individual file imports, obvious type signatures, or boilerplate
- Things already captured in the graph (call `search` first to check)
- Temporary debugging state (unless the debugging revealed an insight)

WRITING STYLE:
- Write in the developer's voice. Be opinionated and specific.
- "This retry logic is fragile because the DLQ silently swallows exceptions
  when full" beats "retry logic exists in billing/retry.rs"
- Include the WHY, not just the WHAT.
- If you have a strong opinion about the code quality, say it.

HOOKS HANDLE:
- Periodic checkpoints (save hook every ~15 messages)
- Emergency flush before context compaction (precompact hook)
- You do NOT need to call `checkpoint` or `snapshot` unless asked."#;

const LEARN_DISABLED_ADDENDUM: &str = r#"

LEARNING MODE: MANUAL
- Do NOT call `learn` unless the developer explicitly asks you to record
  something. You may suggest that something is worth recording."#;

const LEARN_TERSE_ADDENDUM: &str = r#"

LEARNING MODE: TERSE
- Keep learned entries short and factual. Skip narrative and opinion.
- One or two sentences per block maximum."#;

const LEARN_VERBOSE_ADDENDUM: &str = r#"

LEARNING MODE: VERBOSE
- Be thorough in learned entries. Include context, reasoning, and opinion.
- Multiple paragraphs are fine when the insight warrants it."#;

/// Generate the behavioral contract from preferences and current graph state.
///
/// The protocol is mostly static, but the SITUATIONAL CONTEXT section at
/// the end adapts based on the graph's health metrics. This is the
/// mechanism that makes the contract responsive to the current state of
/// the knowledge base rather than being one-size-fits-all.
pub fn generate_protocol(prefs: &Preferences, stats: &TopicStats, has_voice: bool) -> String {
    let mut protocol = DEFAULT_PROTOCOL.to_string();

    if !prefs.learn_auto {
        protocol.push_str(LEARN_DISABLED_ADDENDUM);
    } else {
        match prefs.learn_verbosity.as_str() {
            "terse" => protocol.push_str(LEARN_TERSE_ADDENDUM),
            "verbose" => protocol.push_str(LEARN_VERBOSE_ADDENDUM),
            _ => {} // "normal" — no addendum needed
        }
    }

    if !prefs.prime_include_gotchas {
        protocol.push_str("\n\nNOTE: Gotchas are excluded from `prime` results by preference.");
    }

    // ── Situational context ──────────────────────────────────────
    // Adapt the contract based on graph health metrics.
    let mut situational: Vec<&str> = Vec::new();

    if stats.total < 5 {
        situational.push(
            "BOOTSTRAPPING: This graph is sparse (fewer than 5 topics). Be more \
             aggressive about creating topics as you work — the first few sessions \
             are the most valuable for building the map. Even rough entries are \
             better than nothing; they give future agents a starting point.",
        );
    }

    if stats.total > 0 {
        let stale_pct = (stats.stale_90d as f64 / stats.total as f64) * 100.0;
        if stale_pct > 30.0 {
            situational.push(
                "STALENESS WARNING: Over 30% of topics haven't been touched in 90+ \
                 days. When you prime into an area, verify the returned topics \
                 against the actual code before trusting them. Consider updating \
                 the most relevant stale topics as you encounter them.",
            );
        }
    }

    if !has_voice {
        situational.push(
            "NO VOICE SET: The developer voice is empty. Consider asking the user \
             to set one (`cairn-cli voice set \"...\"` or via the TUI). Without a \
             voice, entries will be written in a generic tone that may not match \
             the developer's style.",
        );
    }

    if !situational.is_empty() {
        protocol.push_str("\n\nSITUATIONAL CONTEXT:");
        for note in situational {
            protocol.push_str("\n- ");
            protocol.push_str(note);
        }
    }

    protocol
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Healthy graph stats for tests that don't care about situational context.
    fn healthy_stats() -> TopicStats {
        TopicStats {
            total: 20,
            active: 18,
            deprecated: 1,
            stale_90d: 1,
        }
    }

    #[test]
    fn test_default_protocol() {
        let prefs = Preferences::default();
        let protocol = generate_protocol(&prefs, &healthy_stats(), true);
        assert!(protocol.contains("ALWAYS:"));
        assert!(protocol.contains("WRITING STYLE:"));
        assert!(protocol.contains("CATALOGUE WHAT YOU CREATE:"));
        assert!(protocol.contains("PERIODIC MAINTENANCE:"));
        assert!(!protocol.contains("LEARNING MODE:"));
        assert!(!protocol.contains("SITUATIONAL CONTEXT"));
    }

    #[test]
    fn test_learn_disabled() {
        let mut prefs = Preferences::default();
        prefs.learn_auto = false;
        let protocol = generate_protocol(&prefs, &healthy_stats(), true);
        assert!(protocol.contains("LEARNING MODE: MANUAL"));
    }

    #[test]
    fn test_learn_terse() {
        let mut prefs = Preferences::default();
        prefs.learn_verbosity = "terse".into();
        let protocol = generate_protocol(&prefs, &healthy_stats(), true);
        assert!(protocol.contains("LEARNING MODE: TERSE"));
    }

    #[test]
    fn test_gotchas_excluded() {
        let mut prefs = Preferences::default();
        prefs.prime_include_gotchas = false;
        let protocol = generate_protocol(&prefs, &healthy_stats(), true);
        assert!(protocol.contains("Gotchas are excluded"));
    }

    #[test]
    fn test_sparse_graph_bootstrapping() {
        let prefs = Preferences::default();
        let sparse = TopicStats {
            total: 2,
            active: 2,
            deprecated: 0,
            stale_90d: 0,
        };
        let protocol = generate_protocol(&prefs, &sparse, true);
        assert!(protocol.contains("BOOTSTRAPPING"));
    }

    #[test]
    fn test_stale_graph_warning() {
        let prefs = Preferences::default();
        let stale = TopicStats {
            total: 10,
            active: 7,
            deprecated: 0,
            stale_90d: 5, // 50% stale
        };
        let protocol = generate_protocol(&prefs, &stale, true);
        assert!(protocol.contains("STALENESS WARNING"));
    }

    #[test]
    fn test_no_voice_warning() {
        let prefs = Preferences::default();
        let protocol = generate_protocol(&prefs, &healthy_stats(), false);
        assert!(protocol.contains("NO VOICE SET"));
    }

    #[test]
    fn test_healthy_graph_no_situational() {
        let prefs = Preferences::default();
        let protocol = generate_protocol(&prefs, &healthy_stats(), true);
        assert!(!protocol.contains("SITUATIONAL CONTEXT"));
    }
}
