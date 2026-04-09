use crate::types::Preferences;

const DEFAULT_PROTOCOL: &str = r#"You have an active Cairn knowledge graph for this workspace.

ALWAYS:
- Call `prime` at the start of every task, passing the task description.
- Call `search` before making architectural recommendations, to check for
  prior context or decisions.

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

RECORD INSIGHTS AND DISCOVERIES:
- A hidden dependency between modules → `connect` with `depends_on`
- A surprising behavior or bug → `learn` under the relevant topic
- A known pitfall → `connect` with `gotcha` edge
- A reason WHY something is built a certain way → `learn` it
- A past incident relevant to current work → `connect` with `war_story`
- That existing knowledge is wrong → `amend` the specific block

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

/// Generate the behavioral contract from preferences.
pub fn generate_protocol(prefs: &Preferences) -> String {
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

    protocol
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_protocol() {
        let prefs = Preferences::default();
        let protocol = generate_protocol(&prefs);
        assert!(protocol.contains("ALWAYS:"));
        assert!(protocol.contains("WRITING STYLE:"));
        assert!(!protocol.contains("LEARNING MODE:"));
    }

    #[test]
    fn test_learn_disabled() {
        let mut prefs = Preferences::default();
        prefs.learn_auto = false;
        let protocol = generate_protocol(&prefs);
        assert!(protocol.contains("LEARNING MODE: MANUAL"));
    }

    #[test]
    fn test_learn_terse() {
        let mut prefs = Preferences::default();
        prefs.learn_verbosity = "terse".into();
        let protocol = generate_protocol(&prefs);
        assert!(protocol.contains("LEARNING MODE: TERSE"));
    }

    #[test]
    fn test_gotchas_excluded() {
        let mut prefs = Preferences::default();
        prefs.prime_include_gotchas = false;
        let protocol = generate_protocol(&prefs);
        assert!(protocol.contains("Gotchas are excluded"));
    }
}
