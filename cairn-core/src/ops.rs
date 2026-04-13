use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use crate::db::CairnDb;
use crate::error::{CairnError, Result};
use crate::types::*;

// ── Internal helpers ─────────────────────────────────────────────

fn generate_block_id() -> String {
    let now = Utc::now().format("%Y%m%d_%H%M%S");
    let short = &Uuid::new_v4().to_string()[..8];
    format!("b_{now}_{short}")
}

fn auto_summary(content: &str, max_len: usize) -> String {
    let trimmed = content.trim();
    if trimmed.len() <= max_len {
        return trimmed.to_string();
    }
    match trimmed[..max_len].rfind(char::is_whitespace) {
        Some(pos) => format!("{}...", &trimmed[..pos]),
        None => format!("{}...", &trimmed[..max_len]),
    }
}

fn make_block(content: &str, voice: Option<&str>) -> Block {
    let now = Utc::now();
    Block {
        id: generate_block_id(),
        content: content.to_string(),
        voice: voice.map(String::from),
        created_at: now,
        updated_at: now,
    }
}

async fn write_history(
    db: &CairnDb,
    op: &str,
    target: &str,
    detail: &str,
    diff: Option<&str>,
) -> Result<()> {
    db.db
        .query(
            "CREATE history SET
                op = $op,
                target = $target,
                detail = $detail,
                diff = $diff,
                created_at = time::now()",
        )
        .bind(("op", op.to_string()))
        .bind(("target", target.to_string()))
        .bind(("detail", detail.to_string()))
        .bind(("diff", diff.map(String::from)))
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;
    Ok(())
}

// ── Helper to fetch a topic by key ───────────────────────────────

#[derive(Debug, Deserialize)]
struct TopicRow {
    key: String,
    title: String,
    summary: String,
    blocks: String,
    tags: Vec<String>,
    created_at: chrono::DateTime<Utc>,
    updated_at: chrono::DateTime<Utc>,
    deprecated: bool,
}

impl TopicRow {
    fn into_topic(self) -> Result<Topic> {
        let blocks: Vec<Block> =
            serde_json::from_str(&self.blocks).map_err(|e| CairnError::Db(e.to_string()))?;
        Ok(Topic {
            key: self.key,
            title: self.title,
            summary: self.summary,
            blocks,
            tags: self.tags,
            created_at: self.created_at,
            updated_at: self.updated_at,
            deprecated: self.deprecated,
        })
    }
}

async fn get_topic(db: &CairnDb, key: &str) -> Result<Option<Topic>> {
    let mut res = db
        .db
        .query("SELECT key, title, summary, blocks, tags, created_at, updated_at, deprecated FROM topic WHERE key = $key LIMIT 1")
        .bind(("key", key.to_string()))
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;

    let row: Option<TopicRow> = res.take(0).map_err(|e| CairnError::Db(e.to_string()))?;
    match row {
        Some(r) => Ok(Some(r.into_topic()?)),
        None => Ok(None),
    }
}

/// Get the SurrealDB record ID string for a topic by its key.
async fn get_topic_record_id(db: &CairnDb, key: &str) -> Result<Option<String>> {
    let mut res = db
        .db
        .query("SELECT VALUE id FROM topic WHERE key = $key LIMIT 1")
        .bind(("key", key.to_string()))
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;

    let thing: Option<surrealdb::sql::Thing> =
        res.take(0).map_err(|e| CairnError::Db(e.to_string()))?;
    Ok(thing.map(|t| t.to_string()))
}

// ── Operations ───────────────────────────────────────────────────

/// Record a new insight or extend an existing topic.
pub async fn learn(db: &CairnDb, params: LearnParams) -> Result<LearnResult> {
    // Reject empty content
    if params.content.trim().is_empty() {
        return Err(CairnError::EmptyContent(
            "learn content must not be empty".into(),
        ));
    }

    let block = make_block(&params.content, params.voice.as_deref());
    let block_id = block.id.clone();

    if let Some(mut topic) = get_topic(db, &params.topic_key).await? {
        // Existing topic — insert block at the requested position
        match params.position {
            Position::Start => topic.blocks.insert(0, block),
            Position::End => topic.blocks.push(block),
            Position::After(ref after_id) => {
                let pos = topic
                    .blocks
                    .iter()
                    .position(|b| b.id == *after_id)
                    .ok_or_else(|| {
                        CairnError::BlockNotFound(after_id.clone(), params.topic_key.clone())
                    })?;
                topic.blocks.insert(pos + 1, block);
            }
        }

        // Append any extra blocks after the primary.
        for eb in &params.extra_blocks {
            topic
                .blocks
                .push(make_block(&eb.content, eb.voice.as_deref()));
        }

        let blocks_json =
            serde_json::to_string(&topic.blocks).map_err(|e| CairnError::Db(e.to_string()))?;
        let block_count = topic.blocks.len();

        // Merge new tags with existing
        let mut all_tags = topic.tags;
        for t in &params.tags {
            if !all_tags.contains(t) {
                all_tags.push(t.clone());
            }
        }

        db.db
            .query(
                "UPDATE topic SET
                    blocks = $blocks,
                    tags = $tags,
                    updated_at = time::now()
                WHERE key = $key",
            )
            .bind(("blocks", blocks_json))
            .bind(("tags", all_tags))
            .bind(("key", params.topic_key.clone()))
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;

        // Update summary if explicitly provided
        if let Some(ref new_summary) = params.summary {
            db.db
                .query("UPDATE topic SET summary = $summary WHERE key = $key")
                .bind(("summary", new_summary.clone()))
                .bind(("key", params.topic_key.clone()))
                .await
                .map_err(|e| CairnError::Db(e.to_string()))?;
        }

        write_history(
            db,
            "learn",
            &format!("topic:{}", params.topic_key),
            &format!("Appended block {block_id}"),
            None,
        )
        .await?;

        Ok(LearnResult {
            topic_key: params.topic_key,
            block_id,
            action: "appended".into(),
            topic_block_count: block_count,
        })
    } else {
        // New topic
        let title = params
            .title
            .unwrap_or_else(|| params.topic_key.replace('-', " "));
        let summary = params
            .summary
            .unwrap_or_else(|| auto_summary(&params.content, 200));

        // Build the full block list: primary + extra.
        let mut all_blocks = vec![block];
        for eb in &params.extra_blocks {
            all_blocks.push(make_block(&eb.content, eb.voice.as_deref()));
        }
        let block_count = all_blocks.len();
        let blocks_json =
            serde_json::to_string(&all_blocks).map_err(|e| CairnError::Db(e.to_string()))?;

        db.db
            .query(
                "CREATE topic SET
                    key = $key,
                    title = $title,
                    summary = $summary,
                    blocks = $blocks,
                    tags = $tags,
                    created_at = time::now(),
                    updated_at = time::now(),
                    deprecated = false",
            )
            .bind(("key", params.topic_key.clone()))
            .bind(("title", title))
            .bind(("summary", summary))
            .bind(("blocks", blocks_json))
            .bind(("tags", params.tags))
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;

        write_history(
            db,
            "learn",
            &format!("topic:{}", params.topic_key),
            &format!(
                "Created topic with {} block(s), first: {block_id}",
                block_count
            ),
            None,
        )
        .await?;

        Ok(LearnResult {
            topic_key: params.topic_key,
            block_id,
            action: "created".into(),
            topic_block_count: block_count,
        })
    }
}

/// Create a typed edge between two topics.
pub async fn connect(db: &CairnDb, params: ConnectParams) -> Result<ConnectResult> {
    let from_id = get_topic_record_id(db, &params.from_key)
        .await?
        .ok_or_else(|| CairnError::TopicNotFound(params.from_key.clone()))?;
    let to_id = get_topic_record_id(db, &params.to_key)
        .await?
        .ok_or_else(|| CairnError::TopicNotFound(params.to_key.clone()))?;

    let table = params.edge_type.table_name();

    // Check for duplicate edge
    let check_query =
        format!("SELECT VALUE id FROM {table} WHERE in = {from_id} AND out = {to_id} LIMIT 1");
    let mut check_res = db
        .db
        .query(&check_query)
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;
    let existing: Option<surrealdb::sql::Thing> = check_res
        .take(0)
        .map_err(|e| CairnError::Db(e.to_string()))?;

    let action;
    if let Some(edge_id) = existing {
        // Update existing edge's note
        let update_query = format!("UPDATE {edge_id} SET note = $note");
        db.db
            .query(&update_query)
            .bind(("note", params.note.clone()))
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;
        action = "updated";
    } else {
        // Create new edge via RELATE
        let severity_clause = if params.edge_type == EdgeKind::Gotcha {
            let sev = params
                .severity
                .as_ref()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "medium".into());
            format!(", severity = '{sev}'")
        } else {
            String::new()
        };

        let relate_query = format!(
            "RELATE {from_id} -> {table} -> {to_id} SET note = $note, created_at = time::now(){severity_clause}"
        );
        db.db
            .query(&relate_query)
            .bind(("note", params.note.clone()))
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;
        action = "created";
    }

    write_history(
        db,
        "connect",
        &format!("{}:{}->{}", table, params.from_key, params.to_key),
        &format!("{action} {table} edge: {}", params.note),
        None,
    )
    .await?;

    Ok(ConnectResult {
        edge: table.into(),
        from: params.from_key,
        to: params.to_key,
        action: action.into(),
        note: params.note,
    })
}

/// Correct or update a specific block within a topic.
pub async fn amend(db: &CairnDb, params: AmendParams) -> Result<AmendResult> {
    let mut topic = get_topic(db, &params.topic_key)
        .await?
        .ok_or_else(|| CairnError::TopicNotFound(params.topic_key.clone()))?;

    let block = topic
        .blocks
        .iter_mut()
        .find(|b| b.id == params.block_id)
        .ok_or_else(|| {
            CairnError::BlockNotFound(params.block_id.clone(), params.topic_key.clone())
        })?;

    let old_content = block.content.clone();
    block.content = params.new_content;
    block.updated_at = Utc::now();

    let blocks_json =
        serde_json::to_string(&topic.blocks).map_err(|e| CairnError::Db(e.to_string()))?;

    db.db
        .query(
            "UPDATE topic SET
                blocks = $blocks,
                updated_at = time::now()
            WHERE key = $key",
        )
        .bind(("blocks", blocks_json))
        .bind(("key", params.topic_key.clone()))
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;

    write_history(
        db,
        "amend",
        &format!("topic:{}", params.topic_key),
        &params.reason,
        Some(&old_content),
    )
    .await?;

    Ok(AmendResult {
        topic_key: params.topic_key,
        block_id: params.block_id,
        action: "amended".into(),
        reason: params.reason,
    })
}

/// Mark a topic as deprecated (soft delete).
pub async fn forget(db: &CairnDb, params: ForgetParams) -> Result<ForgetResult> {
    let _topic = get_topic(db, &params.topic_key)
        .await?
        .ok_or_else(|| CairnError::TopicNotFound(params.topic_key.clone()))?;

    db.db
        .query(
            "UPDATE topic SET
                deprecated = true,
                updated_at = time::now()
            WHERE key = $key",
        )
        .bind(("key", params.topic_key.clone()))
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;

    write_history(
        db,
        "forget",
        &format!("topic:{}", params.topic_key),
        &params.reason,
        None,
    )
    .await?;

    Ok(ForgetResult {
        topic_key: params.topic_key,
        action: "deprecated".into(),
        reason: params.reason,
    })
}

/// Wholesale replacement of a topic's content.
pub async fn rewrite(db: &CairnDb, params: RewriteParams) -> Result<RewriteResult> {
    let topic = get_topic(db, &params.topic_key)
        .await?
        .ok_or_else(|| CairnError::TopicNotFound(params.topic_key.clone()))?;

    let old_block_count = topic.blocks.len();
    let old_blocks_json =
        serde_json::to_string(&topic.blocks).map_err(|e| CairnError::Db(e.to_string()))?;

    let new_blocks: Vec<Block> = params
        .new_blocks
        .iter()
        .map(|nb| make_block(&nb.content, nb.voice.as_deref()))
        .collect();
    let new_block_count = new_blocks.len();

    let blocks_json =
        serde_json::to_string(&new_blocks).map_err(|e| CairnError::Db(e.to_string()))?;

    db.db
        .query(
            "UPDATE topic SET
                blocks = $blocks,
                updated_at = time::now()
            WHERE key = $key",
        )
        .bind(("blocks", blocks_json))
        .bind(("key", params.topic_key.clone()))
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;

    write_history(
        db,
        "rewrite",
        &format!("topic:{}", params.topic_key),
        &params.reason,
        Some(&old_blocks_json),
    )
    .await?;

    Ok(RewriteResult {
        topic_key: params.topic_key,
        action: "rewritten".into(),
        old_block_count,
        new_block_count,
        reason: params.reason,
    })
}

/// Delete all data from the graph (topics, edges, voice, preferences, history).
pub async fn reset(db: &CairnDb) -> Result<()> {
    for table in &[
        "topic",
        "voice",
        "preferences",
        "history",
        "depends_on",
        "contradicts",
        "replaced_by",
        "gotcha",
        "see_also",
        "war_story",
        "owns",
    ] {
        let query = format!("DELETE {table}");
        db.db
            .query(&query)
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;
    }
    Ok(())
}

/// Rename a topic key. Edges are preserved because they reference record IDs, not keys.
pub async fn rename(db: &CairnDb, params: RenameParams) -> Result<RenameResult> {
    let topic = get_topic(db, &params.old_key)
        .await?
        .ok_or_else(|| CairnError::TopicNotFound(params.old_key.clone()))?;

    if get_topic(db, &params.new_key).await?.is_some() {
        return Err(CairnError::TopicKeyConflict(params.new_key.clone()));
    }

    db.db
        .query("UPDATE topic SET key = $new_key, updated_at = time::now() WHERE key = $old_key")
        .bind(("new_key", params.new_key.clone()))
        .bind(("old_key", params.old_key.clone()))
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;

    write_history(
        db,
        "rename",
        &format!("topic:{}", params.new_key),
        &format!("Renamed from '{}' to '{}'", params.old_key, params.new_key),
        None,
    )
    .await?;

    Ok(RenameResult {
        old_key: params.old_key,
        new_key: params.new_key,
        title: topic.title,
    })
}

/// Persist session state and write a session marker.
pub async fn checkpoint(db: &CairnDb, params: CheckpointParams) -> Result<CheckpointResult> {
    // Count mutations in this session
    let mut res = db
        .db
        .query("SELECT count() AS count FROM history WHERE session_id = $sid GROUP ALL")
        .bind(("sid", params.session_id.clone()))
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;

    #[derive(Deserialize)]
    struct CountRow {
        count: usize,
    }
    let count_row: Option<CountRow> = res.take(0).map_err(|e| CairnError::Db(e.to_string()))?;
    let mutations = count_row.map(|r| r.count).unwrap_or(0);

    write_history(
        db,
        "checkpoint",
        "session",
        &format!(
            "Session checkpoint{}",
            if params.emergency { " (emergency)" } else { "" }
        ),
        None,
    )
    .await?;

    Ok(CheckpointResult {
        session_id: params.session_id,
        mutations_persisted: mutations,
        emergency: params.emergency,
    })
}

/// Query the history/audit log.
pub async fn history(db: &CairnDb, params: HistoryParams) -> Result<HistoryResult> {
    let (query, needs_key, needs_session) = match (&params.topic_key, &params.session_id) {
        (Some(_), Some(_)) => (
            "SELECT * FROM history WHERE target CONTAINS $target AND session_id = $sid ORDER BY created_at DESC LIMIT $limit",
            true,
            true,
        ),
        (Some(_), None) => (
            "SELECT * FROM history WHERE target CONTAINS $target ORDER BY created_at DESC LIMIT $limit",
            true,
            false,
        ),
        (None, Some(_)) => (
            "SELECT * FROM history WHERE session_id = $sid ORDER BY created_at DESC LIMIT $limit",
            false,
            true,
        ),
        (None, None) => (
            "SELECT * FROM history ORDER BY created_at DESC LIMIT $limit",
            false,
            false,
        ),
    };

    let mut q = db.db.query(query).bind(("limit", params.limit));

    if needs_key {
        let target = format!("topic:{}", params.topic_key.as_ref().unwrap());
        q = q.bind(("target", target));
    }
    if needs_session {
        q = q.bind(("sid", params.session_id.as_ref().unwrap().clone()));
    }

    let mut res = q.await.map_err(|e| CairnError::Db(e.to_string()))?;
    let events: Vec<HistoryEvent> = res.take(0).map_err(|e| CairnError::Db(e.to_string()))?;

    Ok(HistoryResult { events })
}

// ── Public helper for other modules ──────────────────────────────

/// Fetch a topic by key (used by search, prime, etc.)
pub async fn get_topic_by_key(db: &CairnDb, key: &str) -> Result<Option<Topic>> {
    get_topic(db, key).await
}

// ── New ops (v4) ─────────────────────────────────────────────────

/// Replace a topic's tags wholesale.
pub async fn set_tags(db: &CairnDb, params: SetTagsParams) -> Result<SetTagsResult> {
    let _topic = get_topic(db, &params.topic_key)
        .await?
        .ok_or_else(|| CairnError::TopicNotFound(params.topic_key.clone()))?;

    db.db
        .query(
            "UPDATE topic SET
                tags = $tags,
                updated_at = time::now()
            WHERE key = $key",
        )
        .bind(("tags", params.tags.clone()))
        .bind(("key", params.topic_key.clone()))
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;

    write_history(
        db,
        "set_tags",
        &format!("topic:{}", params.topic_key),
        &format!("tags set to [{}]", params.tags.join(", ")),
        None,
    )
    .await?;

    Ok(SetTagsResult {
        topic_key: params.topic_key,
        tags: params.tags,
    })
}

/// Replace a topic's summary.
pub async fn set_summary(db: &CairnDb, params: SetSummaryParams) -> Result<SetSummaryResult> {
    let _topic = get_topic(db, &params.topic_key)
        .await?
        .ok_or_else(|| CairnError::TopicNotFound(params.topic_key.clone()))?;

    db.db
        .query(
            "UPDATE topic SET
                summary = $summary,
                updated_at = time::now()
            WHERE key = $key",
        )
        .bind(("summary", params.summary.clone()))
        .bind(("key", params.topic_key.clone()))
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;

    write_history(
        db,
        "set_summary",
        &format!("topic:{}", params.topic_key),
        "summary updated",
        None,
    )
    .await?;

    Ok(SetSummaryResult {
        topic_key: params.topic_key,
        summary: params.summary,
    })
}

/// Remove a single edge between two topics.
pub async fn disconnect(db: &CairnDb, params: DisconnectParams) -> Result<DisconnectResult> {
    let from_id = get_topic_record_id(db, &params.from_key)
        .await?
        .ok_or_else(|| CairnError::TopicNotFound(params.from_key.clone()))?;
    let to_id = get_topic_record_id(db, &params.to_key)
        .await?
        .ok_or_else(|| CairnError::TopicNotFound(params.to_key.clone()))?;

    let table = params.edge_type.table_name();

    // Find the edge.
    let query =
        format!("SELECT VALUE id FROM {table} WHERE in = {from_id} AND out = {to_id} LIMIT 1");
    let mut res = db
        .db
        .query(&query)
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;
    let existing: Option<surrealdb::sql::Thing> =
        res.take(0).map_err(|e| CairnError::Db(e.to_string()))?;

    let action = if let Some(edge_id) = existing {
        let del = format!("DELETE {edge_id}");
        db.db
            .query(&del)
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;
        "deleted"
    } else {
        "not_found"
    };

    write_history(
        db,
        "disconnect",
        &format!("{}:{}->{}", table, params.from_key, params.to_key),
        &format!("{action} {table} edge"),
        None,
    )
    .await?;

    Ok(DisconnectResult {
        edge: table.into(),
        from: params.from_key,
        to: params.to_key,
        action: action.into(),
    })
}

/// Delete a block from a topic.
pub async fn delete_block(db: &CairnDb, params: DeleteBlockParams) -> Result<DeleteBlockResult> {
    let mut topic = get_topic(db, &params.topic_key)
        .await?
        .ok_or_else(|| CairnError::TopicNotFound(params.topic_key.clone()))?;

    let idx = topic
        .blocks
        .iter()
        .position(|b| b.id == params.block_id)
        .ok_or_else(|| {
            CairnError::BlockNotFound(params.block_id.clone(), params.topic_key.clone())
        })?;
    let removed = topic.blocks.remove(idx);

    let blocks_json =
        serde_json::to_string(&topic.blocks).map_err(|e| CairnError::Db(e.to_string()))?;

    db.db
        .query(
            "UPDATE topic SET
                blocks = $blocks,
                updated_at = time::now()
            WHERE key = $key",
        )
        .bind(("blocks", blocks_json))
        .bind(("key", params.topic_key.clone()))
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;

    write_history(
        db,
        "delete_block",
        &format!("topic:{}", params.topic_key),
        &params.reason,
        Some(&removed.content),
    )
    .await?;

    Ok(DeleteBlockResult {
        topic_key: params.topic_key,
        block_id: params.block_id,
        remaining_blocks: topic.blocks.len(),
    })
}

/// Rewrite multiple topics in a single call. Each entry is processed
/// sequentially under the same lock, so it's atomic from other clients'
/// perspective. Errors on individual entries don't abort the batch —
/// they're collected in the results.
pub async fn batch_rewrite(db: &CairnDb, params: BatchRewriteParams) -> Result<BatchRewriteResult> {
    let mut results = Vec::new();
    let mut succeeded = 0;

    for entry in params.entries {
        let r = rewrite(
            db,
            RewriteParams {
                topic_key: entry.topic_key,
                new_blocks: entry.new_blocks,
                reason: entry.reason,
            },
        )
        .await;
        match r {
            Ok(result) => {
                succeeded += 1;
                results.push(result);
            }
            Err(e) => {
                // Record the failure as a RewriteResult with action="error".
                results.push(RewriteResult {
                    topic_key: "".into(),
                    action: format!("error: {e}"),
                    old_block_count: 0,
                    new_block_count: 0,
                    reason: e.to_string(),
                });
            }
        }
    }

    let total = results.len();
    Ok(BatchRewriteResult {
        results,
        total,
        succeeded,
    })
}

/// Move a block to a new position within its topic.
pub async fn move_block(db: &CairnDb, params: MoveBlockParams) -> Result<MoveBlockResult> {
    let mut topic = get_topic(db, &params.topic_key)
        .await?
        .ok_or_else(|| CairnError::TopicNotFound(params.topic_key.clone()))?;

    // Find and remove the block from its current position.
    let idx = topic
        .blocks
        .iter()
        .position(|b| b.id == params.block_id)
        .ok_or_else(|| {
            CairnError::BlockNotFound(params.block_id.clone(), params.topic_key.clone())
        })?;
    let block = topic.blocks.remove(idx);

    // Insert at the new position.
    let new_idx = match &params.position {
        Position::Start => 0,
        Position::End => topic.blocks.len(),
        Position::After(after_id) => {
            let after_idx = topic
                .blocks
                .iter()
                .position(|b| b.id == *after_id)
                .ok_or_else(|| {
                    CairnError::BlockNotFound(after_id.clone(), params.topic_key.clone())
                })?;
            after_idx + 1
        }
    };
    topic.blocks.insert(new_idx, block);

    let blocks_json =
        serde_json::to_string(&topic.blocks).map_err(|e| CairnError::Db(e.to_string()))?;

    db.db
        .query(
            "UPDATE topic SET
                blocks = $blocks,
                updated_at = time::now()
            WHERE key = $key",
        )
        .bind(("blocks", blocks_json))
        .bind(("key", params.topic_key.clone()))
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;

    write_history(
        db,
        "move_block",
        &format!("topic:{}", params.topic_key),
        &format!("moved block {} to position {}", params.block_id, new_idx),
        None,
    )
    .await?;

    Ok(MoveBlockResult {
        topic_key: params.topic_key,
        block_id: params.block_id,
        new_position: new_idx,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> CairnDb {
        CairnDb::open_memory().await.unwrap()
    }

    #[tokio::test]
    async fn test_learn_create() {
        let db = test_db().await;
        let result = learn(
            &db,
            LearnParams {
                topic_key: "billing-retry".into(),
                title: Some("Payment retry mechanism".into()),
                summary: None,
                content: "The retry logic is fragile because the DLQ silently swallows exceptions."
                    .into(),
                voice: Some("frustrated".into()),
                tags: vec!["billing".into(), "retry".into()],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        assert_eq!(result.action, "created");
        assert_eq!(result.topic_block_count, 1);

        let topic = get_topic(&db, "billing-retry").await.unwrap().unwrap();
        assert_eq!(topic.title, "Payment retry mechanism");
        assert_eq!(topic.blocks.len(), 1);
        assert_eq!(topic.blocks[0].voice.as_deref(), Some("frustrated"));
        assert_eq!(topic.tags, vec!["billing", "retry"]);
    }

    #[tokio::test]
    async fn test_learn_append() {
        let db = test_db().await;

        learn(
            &db,
            LearnParams {
                topic_key: "test-topic".into(),
                title: Some("Test".into()),
                summary: None,
                content: "First block".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        let result = learn(
            &db,
            LearnParams {
                topic_key: "test-topic".into(),
                title: None,
                summary: None,
                content: "Second block".into(),
                voice: None,
                tags: vec!["new-tag".into()],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        assert_eq!(result.action, "appended");
        assert_eq!(result.topic_block_count, 2);

        let topic = get_topic(&db, "test-topic").await.unwrap().unwrap();
        assert_eq!(topic.blocks.len(), 2);
        assert_eq!(topic.blocks[0].content, "First block");
        assert_eq!(topic.blocks[1].content, "Second block");
        assert!(topic.tags.contains(&"new-tag".to_string()));
    }

    #[tokio::test]
    async fn test_learn_insert_at_start() {
        let db = test_db().await;

        learn(
            &db,
            LearnParams {
                topic_key: "t".into(),
                title: Some("T".into()),
                summary: None,
                content: "Original".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        learn(
            &db,
            LearnParams {
                topic_key: "t".into(),
                title: None,
                summary: None,
                content: "Prepended".into(),
                voice: None,
                tags: vec![],
                position: Position::Start,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        let topic = get_topic(&db, "t").await.unwrap().unwrap();
        assert_eq!(topic.blocks[0].content, "Prepended");
        assert_eq!(topic.blocks[1].content, "Original");
    }

    #[tokio::test]
    async fn test_connect() {
        let db = test_db().await;

        // Create two topics
        for key in &["topic-a", "topic-b"] {
            learn(
                &db,
                LearnParams {
                    topic_key: key.to_string(),
                    title: Some(format!("Topic {key}")),
                    summary: None,
                    content: "content".into(),
                    voice: None,
                    tags: vec![],
                    position: Position::End,
                    extra_blocks: vec![],
                },
            )
            .await
            .unwrap();
        }

        let result = connect(
            &db,
            ConnectParams {
                from_key: "topic-a".into(),
                to_key: "topic-b".into(),
                edge_type: EdgeKind::DependsOn,
                note: "A depends on B".into(),
                severity: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(result.action, "created");
        assert_eq!(result.edge, "depends_on");
    }

    #[tokio::test]
    async fn test_connect_duplicate_updates() {
        let db = test_db().await;

        for key in &["x", "y"] {
            learn(
                &db,
                LearnParams {
                    topic_key: key.to_string(),
                    title: Some(key.to_string()),
                    summary: None,
                    content: "c".into(),
                    voice: None,
                    tags: vec![],
                    position: Position::End,
                    extra_blocks: vec![],
                },
            )
            .await
            .unwrap();
        }

        connect(
            &db,
            ConnectParams {
                from_key: "x".into(),
                to_key: "y".into(),
                edge_type: EdgeKind::SeeAlso,
                note: "original note".into(),
                severity: None,
            },
        )
        .await
        .unwrap();

        let result = connect(
            &db,
            ConnectParams {
                from_key: "x".into(),
                to_key: "y".into(),
                edge_type: EdgeKind::SeeAlso,
                note: "updated note".into(),
                severity: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(result.action, "updated");
    }

    #[tokio::test]
    async fn test_connect_missing_topic() {
        let db = test_db().await;

        learn(
            &db,
            LearnParams {
                topic_key: "exists".into(),
                title: Some("Exists".into()),
                summary: None,
                content: "c".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        let err = connect(
            &db,
            ConnectParams {
                from_key: "exists".into(),
                to_key: "missing".into(),
                edge_type: EdgeKind::DependsOn,
                note: "n".into(),
                severity: None,
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, CairnError::TopicNotFound(_)));
    }

    #[tokio::test]
    async fn test_amend() {
        let db = test_db().await;

        let lr = learn(
            &db,
            LearnParams {
                topic_key: "amend-me".into(),
                title: Some("Amend Me".into()),
                summary: None,
                content: "old content".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        let result = amend(
            &db,
            AmendParams {
                topic_key: "amend-me".into(),
                block_id: lr.block_id.clone(),
                new_content: "new content".into(),
                reason: "was wrong".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(result.action, "amended");

        let topic = get_topic(&db, "amend-me").await.unwrap().unwrap();
        assert_eq!(topic.blocks[0].content, "new content");
    }

    #[tokio::test]
    async fn test_forget() {
        let db = test_db().await;

        learn(
            &db,
            LearnParams {
                topic_key: "forget-me".into(),
                title: Some("Forget Me".into()),
                summary: None,
                content: "c".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        let result = forget(
            &db,
            ForgetParams {
                topic_key: "forget-me".into(),
                reason: "no longer relevant".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(result.action, "deprecated");

        let topic = get_topic(&db, "forget-me").await.unwrap().unwrap();
        assert!(topic.deprecated);
    }

    #[tokio::test]
    async fn test_rewrite() {
        let db = test_db().await;

        learn(
            &db,
            LearnParams {
                topic_key: "rewrite-me".into(),
                title: Some("Rewrite Me".into()),
                summary: None,
                content: "block 1".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        learn(
            &db,
            LearnParams {
                topic_key: "rewrite-me".into(),
                title: None,
                summary: None,
                content: "block 2".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        let result = rewrite(
            &db,
            RewriteParams {
                topic_key: "rewrite-me".into(),
                new_blocks: vec![NewBlock {
                    content: "completely new content".into(),
                    voice: Some("confident".into()),
                }],
                reason: "total redesign".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(result.old_block_count, 2);
        assert_eq!(result.new_block_count, 1);

        let topic = get_topic(&db, "rewrite-me").await.unwrap().unwrap();
        assert_eq!(topic.blocks.len(), 1);
        assert_eq!(topic.blocks[0].content, "completely new content");
    }

    #[tokio::test]
    async fn test_history() {
        let db = test_db().await;

        learn(
            &db,
            LearnParams {
                topic_key: "hist-test".into(),
                title: Some("History Test".into()),
                summary: None,
                content: "c".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        let result = history(
            &db,
            HistoryParams {
                topic_key: Some("hist-test".into()),
                limit: 10,
                session_id: None,
            },
        )
        .await
        .unwrap();

        assert!(!result.events.is_empty());
        assert_eq!(result.events[0].op, "learn");
    }

    #[tokio::test]
    async fn test_checkpoint() {
        let db = test_db().await;

        let result = checkpoint(
            &db,
            CheckpointParams {
                session_id: "test-session".into(),
                emergency: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(result.session_id, "test-session");
        assert!(!result.emergency);
    }

    #[tokio::test]
    async fn test_learn_empty_content_rejected() {
        let db = test_db().await;

        let err = learn(
            &db,
            LearnParams {
                topic_key: "empty".into(),
                title: Some("Empty".into()),
                summary: None,
                content: "".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, CairnError::EmptyContent(_)));

        // Whitespace-only should also be rejected
        let err = learn(
            &db,
            LearnParams {
                topic_key: "empty".into(),
                title: Some("Empty".into()),
                summary: None,
                content: "   \n\t  ".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, CairnError::EmptyContent(_)));
    }

    #[tokio::test]
    async fn test_learn_auto_summary() {
        let db = test_db().await;

        learn(
            &db,
            LearnParams {
                topic_key: "auto-sum".into(),
                title: Some("Auto Summary".into()),
                summary: None,
                content: "This is a fairly long insight about how the system works.".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        let topic = get_topic(&db, "auto-sum").await.unwrap().unwrap();
        assert!(!topic.summary.is_empty());
        assert!(topic.summary.contains("fairly long insight"));
    }

    #[tokio::test]
    async fn test_learn_explicit_summary() {
        let db = test_db().await;

        learn(
            &db,
            LearnParams {
                topic_key: "explicit-sum".into(),
                title: Some("Explicit Summary".into()),
                summary: Some("My custom summary".into()),
                content: "The actual block content which is different.".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        let topic = get_topic(&db, "explicit-sum").await.unwrap().unwrap();
        assert_eq!(topic.summary, "My custom summary");
    }

    #[tokio::test]
    async fn test_learn_summary_update_on_append() {
        let db = test_db().await;

        learn(
            &db,
            LearnParams {
                topic_key: "sum-update".into(),
                title: Some("Summary Update".into()),
                summary: Some("Original summary".into()),
                content: "First block".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        // Append with new summary
        learn(
            &db,
            LearnParams {
                topic_key: "sum-update".into(),
                title: None,
                summary: Some("Updated summary".into()),
                content: "Second block".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        let topic = get_topic(&db, "sum-update").await.unwrap().unwrap();
        assert_eq!(topic.summary, "Updated summary");
    }

    #[tokio::test]
    async fn test_learn_summary_preserved_on_append() {
        let db = test_db().await;

        learn(
            &db,
            LearnParams {
                topic_key: "sum-keep".into(),
                title: Some("Summary Keep".into()),
                summary: Some("Original summary".into()),
                content: "First block".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        // Append without summary — should keep the original
        learn(
            &db,
            LearnParams {
                topic_key: "sum-keep".into(),
                title: None,
                summary: None,
                content: "Second block".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        let topic = get_topic(&db, "sum-keep").await.unwrap().unwrap();
        assert_eq!(topic.summary, "Original summary");
    }

    #[tokio::test]
    async fn test_rename_basic() {
        let db = test_db().await;

        learn(
            &db,
            LearnParams {
                topic_key: "old-name".into(),
                title: Some("My Topic".into()),
                summary: None,
                content: "Some content".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        let result = rename(
            &db,
            RenameParams {
                old_key: "old-name".into(),
                new_key: "new-name".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(result.old_key, "old-name");
        assert_eq!(result.new_key, "new-name");
        assert_eq!(result.title, "My Topic");

        // Old key should be gone, new key should exist
        assert!(get_topic(&db, "old-name").await.unwrap().is_none());
        let topic = get_topic(&db, "new-name").await.unwrap().unwrap();
        assert_eq!(topic.title, "My Topic");
        assert_eq!(topic.blocks.len(), 1);
    }

    #[tokio::test]
    async fn test_rename_missing_key() {
        let db = test_db().await;

        let err = rename(
            &db,
            RenameParams {
                old_key: "nonexistent".into(),
                new_key: "whatever".into(),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, CairnError::TopicNotFound(_)));
    }

    #[tokio::test]
    async fn test_rename_conflict() {
        let db = test_db().await;

        for key in &["a", "b"] {
            learn(
                &db,
                LearnParams {
                    topic_key: key.to_string(),
                    title: Some(key.to_string()),
                    summary: None,
                    content: "c".into(),
                    voice: None,
                    tags: vec![],
                    position: Position::End,
                    extra_blocks: vec![],
                },
            )
            .await
            .unwrap();
        }

        let err = rename(
            &db,
            RenameParams {
                old_key: "a".into(),
                new_key: "b".into(),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, CairnError::TopicKeyConflict(_)));
    }

    #[tokio::test]
    async fn test_rename_edges_preserved() {
        let db = test_db().await;

        for key in &["src", "dst"] {
            learn(
                &db,
                LearnParams {
                    topic_key: key.to_string(),
                    title: Some(key.to_string()),
                    summary: None,
                    content: "c".into(),
                    voice: None,
                    tags: vec![],
                    position: Position::End,
                    extra_blocks: vec![],
                },
            )
            .await
            .unwrap();
        }

        connect(
            &db,
            ConnectParams {
                from_key: "src".into(),
                to_key: "dst".into(),
                edge_type: EdgeKind::DependsOn,
                note: "test edge".into(),
                severity: None,
            },
        )
        .await
        .unwrap();

        // Rename the source topic
        rename(
            &db,
            RenameParams {
                old_key: "src".into(),
                new_key: "renamed-src".into(),
            },
        )
        .await
        .unwrap();

        // Edge should still be discoverable via the search module
        let edges = crate::search::explore(
            &db,
            ExploreParams {
                topic_key: "renamed-src".into(),
                depth: 1,
                edge_types: vec![],
            },
        )
        .await
        .unwrap();

        assert!(!edges.edges.is_empty());
        assert!(edges
            .edges
            .iter()
            .any(|e| e.from == "renamed-src" && e.to == "dst"));
    }
}
