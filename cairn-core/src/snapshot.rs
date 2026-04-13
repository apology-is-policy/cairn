use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::db::CairnDb;
use crate::error::{CairnError, Result};
use crate::types::*;

// ── Manifest ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotEntry {
    name: String,
    path: String,
    size_bytes: u64,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Manifest {
    snapshots: Vec<SnapshotEntry>,
}

fn snapshots_dir(base: Option<&str>) -> PathBuf {
    match base {
        Some(p) => PathBuf::from(p),
        None => {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".cairn").join("snapshots")
        }
    }
}

fn manifest_path(snapshots_dir: &Path) -> PathBuf {
    snapshots_dir.join("manifest.json")
}

fn read_manifest(snapshots_dir: &Path) -> Result<Manifest> {
    let path = manifest_path(snapshots_dir);
    if !path.exists() {
        return Ok(Manifest::default());
    }
    let data = std::fs::read_to_string(&path)?;
    serde_json::from_str(&data).map_err(|e| CairnError::Db(format!("Manifest parse error: {e}")))
}

fn write_manifest(snapshots_dir: &Path, manifest: &Manifest) -> Result<()> {
    let path = manifest_path(snapshots_dir);
    let data = serde_json::to_string_pretty(manifest)
        .map_err(|e| CairnError::Db(format!("Manifest serialize error: {e}")))?;
    std::fs::write(&path, data)?;
    Ok(())
}

// ── Export helper ────────────────────────────────────────────────

/// Export the full graph as a JSON structure (all topics, edges, voice, preferences, history).
async fn export_graph(db: &CairnDb) -> Result<serde_json::Value> {
    // Export topics
    let mut topic_res = db
        .db
        .query("SELECT key, title, summary, blocks, tags, created_at, updated_at, deprecated FROM topic")
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;
    let topics: Vec<serde_json::Value> = topic_res
        .take(0)
        .map_err(|e| CairnError::Db(e.to_string()))?;

    // Export edges (as note + from/to keys)
    let mut edges_export = Vec::new();
    for kind in crate::types::EdgeKind::ALL {
        let table = kind.table_name();
        // Get edges with resolved keys
        let query = format!("SELECT note FROM {table}");
        let mut res = db
            .db
            .query(&query)
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;
        let edge_notes: Vec<serde_json::Value> =
            res.take(0).map_err(|e| CairnError::Db(e.to_string()))?;

        // Also get in/out record IDs
        let id_query = format!("SELECT VALUE id FROM {table}");
        let mut id_res = db
            .db
            .query(&id_query)
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;
        let ids: Vec<surrealdb::sql::Thing> =
            id_res.take(0).map_err(|e| CairnError::Db(e.to_string()))?;

        // Get in/out for each edge
        #[derive(Deserialize)]
        struct EdgeRef {
            #[serde(rename = "in")]
            in_id: surrealdb::sql::Thing,
            out: surrealdb::sql::Thing,
            note: String,
        }

        let ref_query = format!("SELECT in, out, note FROM {table}");
        let mut ref_res = db
            .db
            .query(&ref_query)
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;
        let refs: Vec<EdgeRef> = ref_res.take(0).map_err(|e| CairnError::Db(e.to_string()))?;

        // Build id-to-key map
        let mut key_res = db
            .db
            .query("SELECT VALUE id FROM topic")
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;
        let topic_ids: Vec<surrealdb::sql::Thing> =
            key_res.take(0).map_err(|e| CairnError::Db(e.to_string()))?;
        let mut key_res2 = db
            .db
            .query("SELECT VALUE key FROM topic")
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;
        let topic_keys: Vec<String> = key_res2
            .take(0)
            .map_err(|e| CairnError::Db(e.to_string()))?;

        let id_key: std::collections::HashMap<String, String> = topic_ids
            .into_iter()
            .zip(topic_keys)
            .map(|(id, key)| (id.to_string(), key))
            .collect();

        for edge_ref in &refs {
            let from_key = id_key
                .get(&edge_ref.in_id.to_string())
                .cloned()
                .unwrap_or_default();
            let to_key = id_key
                .get(&edge_ref.out.to_string())
                .cloned()
                .unwrap_or_default();

            edges_export.push(serde_json::json!({
                "type": table,
                "from": from_key,
                "to": to_key,
                "note": edge_ref.note,
            }));
        }

        let _ = edge_notes;
        let _ = ids;
    }

    // Export voice
    let mut voice_res = db
        .db
        .query("SELECT content, updated_at FROM voice LIMIT 1")
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;
    let voice: Option<serde_json::Value> = voice_res
        .take(0)
        .map_err(|e| CairnError::Db(e.to_string()))?;

    // Export preferences
    let mut prefs_res = db
        .db
        .query("SELECT prime_max_tokens, prime_include_gotchas, learn_verbosity, learn_auto, updated_at FROM preferences LIMIT 1")
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;
    let prefs: Option<serde_json::Value> = prefs_res
        .take(0)
        .map_err(|e| CairnError::Db(e.to_string()))?;

    // Export history
    let mut hist_res = db
        .db
        .query("SELECT op, target, detail, diff, session_id, created_at FROM history ORDER BY created_at ASC")
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;
    let history: Vec<serde_json::Value> = hist_res
        .take(0)
        .map_err(|e| CairnError::Db(e.to_string()))?;

    Ok(serde_json::json!({
        "version": 1,
        "exported_at": Utc::now().to_rfc3339(),
        "topics": topics,
        "edges": edges_export,
        "voice": voice,
        "preferences": prefs,
        "history": history,
    }))
}

/// Import a graph from a JSON export, clearing existing data first.
async fn import_graph(db: &CairnDb, data: &serde_json::Value) -> Result<(usize, usize)> {
    // Clear existing data
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

    // Import topics
    let mut topic_count = 0;
    if let Some(topics) = data.get("topics").and_then(|v| v.as_array()) {
        for topic in topics {
            db.db
                .query(
                    "CREATE topic SET
                        key = $key, title = $title, summary = $summary,
                        blocks = $blocks, tags = $tags,
                        created_at = time::now(), updated_at = time::now(),
                        deprecated = $deprecated",
                )
                .bind(("key", topic["key"].as_str().unwrap_or_default().to_string()))
                .bind((
                    "title",
                    topic["title"].as_str().unwrap_or_default().to_string(),
                ))
                .bind((
                    "summary",
                    topic["summary"].as_str().unwrap_or_default().to_string(),
                ))
                .bind((
                    "blocks",
                    topic["blocks"].as_str().unwrap_or("[]").to_string(),
                ))
                .bind((
                    "tags",
                    topic["tags"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str())
                                .map(String::from)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default(),
                ))
                .bind(("deprecated", topic["deprecated"].as_bool().unwrap_or(false)))
                .await
                .map_err(|e| CairnError::Db(e.to_string()))?;
            topic_count += 1;
        }
    }

    // Import edges
    let mut edge_count = 0;
    if let Some(edges) = data.get("edges").and_then(|v| v.as_array()) {
        for edge in edges {
            let edge_type = edge["type"].as_str().unwrap_or_default();
            let from_key = edge["from"].as_str().unwrap_or_default();
            let to_key = edge["to"].as_str().unwrap_or_default();
            let note = edge["note"].as_str().unwrap_or_default();

            // Get record IDs for the topics
            let mut from_res = db
                .db
                .query("SELECT VALUE id FROM topic WHERE key = $key LIMIT 1")
                .bind(("key", from_key.to_string()))
                .await
                .map_err(|e| CairnError::Db(e.to_string()))?;
            let from_id: Option<surrealdb::sql::Thing> = from_res
                .take(0)
                .map_err(|e| CairnError::Db(e.to_string()))?;

            let mut to_res = db
                .db
                .query("SELECT VALUE id FROM topic WHERE key = $key LIMIT 1")
                .bind(("key", to_key.to_string()))
                .await
                .map_err(|e| CairnError::Db(e.to_string()))?;
            let to_id: Option<surrealdb::sql::Thing> =
                to_res.take(0).map_err(|e| CairnError::Db(e.to_string()))?;

            if let (Some(from), Some(to)) = (from_id, to_id) {
                let query = format!(
                    "RELATE {} -> {} -> {} SET note = $note, created_at = time::now()",
                    from, edge_type, to
                );
                db.db
                    .query(&query)
                    .bind(("note", note.to_string()))
                    .await
                    .map_err(|e| CairnError::Db(e.to_string()))?;
                edge_count += 1;
            }
        }
    }

    // Import voice
    if let Some(voice) = data.get("voice").and_then(|v| v.as_object()) {
        if let Some(content) = voice.get("content").and_then(|v| v.as_str()) {
            crate::prime::set_voice(db, content).await?;
        }
    }

    // Import preferences
    if let Some(prefs) = data.get("preferences").and_then(|v| v.as_object()) {
        let p = Preferences {
            prime_max_tokens: prefs
                .get("prime_max_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(4000),
            prime_include_gotchas: prefs
                .get("prime_include_gotchas")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            learn_verbosity: prefs
                .get("learn_verbosity")
                .and_then(|v| v.as_str())
                .unwrap_or("normal")
                .to_string(),
            learn_auto: prefs
                .get("learn_auto")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            updated_at: Utc::now(),
        };
        crate::prime::set_preferences(db, &p).await?;
    }

    // Import history
    if let Some(history) = data.get("history").and_then(|v| v.as_array()) {
        for event in history {
            db.db
                .query(
                    "CREATE history SET
                        op = $op, target = $target, detail = $detail,
                        diff = $diff, session_id = $sid, created_at = time::now()",
                )
                .bind(("op", event["op"].as_str().unwrap_or_default().to_string()))
                .bind((
                    "target",
                    event["target"].as_str().unwrap_or_default().to_string(),
                ))
                .bind((
                    "detail",
                    event["detail"].as_str().unwrap_or_default().to_string(),
                ))
                .bind(("diff", event["diff"].as_str().map(String::from)))
                .bind((
                    "sid",
                    event["session_id"].as_str().unwrap_or_default().to_string(),
                ))
                .await
                .map_err(|e| CairnError::Db(e.to_string()))?;
        }
    }

    Ok((topic_count, edge_count))
}

// ── Public operations ────────────────────────────────────────────

/// Create a named, full backup of the database.
pub async fn snapshot(db: &CairnDb, params: SnapshotParams) -> Result<SnapshotResult> {
    let dir = snapshots_dir(params.path.as_deref());
    std::fs::create_dir_all(&dir)?;

    let name = params
        .name
        .unwrap_or_else(|| Utc::now().format("snapshot_%Y%m%d_%H%M%S").to_string());
    let file_path = dir.join(format!("{name}.json"));

    let data = export_graph(db).await?;
    let json = serde_json::to_string_pretty(&data)
        .map_err(|e| CairnError::Db(format!("Serialize error: {e}")))?;

    std::fs::write(&file_path, &json)?;
    let size_bytes = json.len() as u64;

    // Update manifest
    let mut manifest = read_manifest(&dir)?;
    manifest.snapshots.push(SnapshotEntry {
        name: name.clone(),
        path: file_path.display().to_string(),
        size_bytes,
        created_at: Utc::now(),
    });
    write_manifest(&dir, &manifest)?;

    Ok(SnapshotResult {
        name,
        path: file_path.display().to_string(),
        size_bytes,
        created_at: Utc::now(),
    })
}

/// Restore the database from a named snapshot.
pub async fn restore(db: &CairnDb, params: RestoreParams) -> Result<RestoreResult> {
    let dir = snapshots_dir(None);
    let manifest = read_manifest(&dir)?;

    let entry = manifest
        .snapshots
        .iter()
        .find(|e| e.name == params.name)
        .ok_or_else(|| CairnError::SnapshotNotFound(params.name.clone()))?;

    let snapshot_path = PathBuf::from(&entry.path);
    if !snapshot_path.exists() {
        return Err(CairnError::SnapshotNotFound(format!(
            "File not found: {}",
            entry.path
        )));
    }

    // Auto-create safety snapshot
    let safety_name = Utc::now().format("pre_restore_%Y%m%d_%H%M%S").to_string();
    snapshot(
        db,
        SnapshotParams {
            name: Some(safety_name.clone()),
            path: None,
        },
    )
    .await?;

    // Read and import
    let json = std::fs::read_to_string(&snapshot_path)?;
    let data: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| CairnError::Db(format!("Parse error: {e}")))?;

    let (topics_restored, edges_restored) = import_graph(db, &data).await?;

    Ok(RestoreResult {
        restored_from: params.name,
        safety_snapshot: safety_name,
        topics_restored,
        edges_restored,
    })
}

/// List all snapshots from the manifest.
pub fn list_snapshots() -> Result<Vec<SnapshotResult>> {
    let dir = snapshots_dir(None);
    let manifest = read_manifest(&dir)?;

    Ok(manifest
        .snapshots
        .into_iter()
        .map(|e| SnapshotResult {
            name: e.name,
            path: e.path,
            size_bytes: e.size_bytes,
            created_at: e.created_at,
        })
        .collect())
}

/// Export the full graph as JSON (for migration or human-readable dump).
pub async fn export_json(db: &CairnDb) -> Result<String> {
    let data = export_graph(db).await?;
    serde_json::to_string_pretty(&data).map_err(|e| CairnError::Db(format!("Serialize error: {e}")))
}

/// Import from a JSON export string.
pub async fn import_json(db: &CairnDb, json: &str) -> Result<(usize, usize)> {
    let data: serde_json::Value =
        serde_json::from_str(json).map_err(|e| CairnError::Db(format!("Parse error: {e}")))?;
    import_graph(db, &data).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops;

    async fn test_db() -> CairnDb {
        CairnDb::open_memory().await.unwrap()
    }

    #[tokio::test]
    async fn test_export_import_roundtrip() {
        let db = test_db().await;

        // Setup data
        crate::prime::init_defaults(&db, Some("I love Rust."))
            .await
            .unwrap();

        ops::learn(
            &db,
            LearnParams {
                topic_key: "test-topic".into(),
                title: Some("Test Topic".into()),
                summary: None,
                content: "Some insight here.".into(),
                voice: None,
                tags: vec!["test".into()],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        ops::learn(
            &db,
            LearnParams {
                topic_key: "other-topic".into(),
                title: Some("Other Topic".into()),
                summary: None,
                content: "Another insight.".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        ops::connect(
            &db,
            ConnectParams {
                from_key: "test-topic".into(),
                to_key: "other-topic".into(),
                edge_type: EdgeKind::SeeAlso,
                note: "related stuff".into(),
                severity: None,
            },
        )
        .await
        .unwrap();

        // Export
        let json = export_json(&db).await.unwrap();
        let data: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(data["version"], 1);
        assert_eq!(data["topics"].as_array().unwrap().len(), 2);
        assert_eq!(data["edges"].as_array().unwrap().len(), 1);
        assert!(data["voice"].is_object());

        // Import into a fresh DB
        let db2 = test_db().await;
        let (topics, edges) = import_json(&db2, &json).await.unwrap();

        assert_eq!(topics, 2);
        assert_eq!(edges, 1);

        // Verify imported data
        let topic = ops::get_topic_by_key(&db2, "test-topic")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(topic.title, "Test Topic");
        assert_eq!(topic.blocks.len(), 1);

        let voice = crate::prime::get_voice(&db2).await.unwrap().unwrap();
        assert_eq!(voice.content, "I love Rust.");
    }

    #[tokio::test]
    async fn test_snapshot_to_dir() {
        let db = test_db().await;

        ops::learn(
            &db,
            LearnParams {
                topic_key: "snap-test".into(),
                title: Some("Snapshot Test".into()),
                summary: None,
                content: "data".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
                extra_blocks: vec![],
            },
        )
        .await
        .unwrap();

        // Snapshot to a temp directory
        let tmp_dir = std::env::temp_dir().join("cairn_test_snapshots");
        let _ = std::fs::remove_dir_all(&tmp_dir);

        let result = snapshot(
            &db,
            SnapshotParams {
                name: Some("test-snap".into()),
                path: Some(tmp_dir.display().to_string()),
            },
        )
        .await
        .unwrap();

        assert_eq!(result.name, "test-snap");
        assert!(result.size_bytes > 0);
        assert!(PathBuf::from(&result.path).exists());

        // Check manifest
        let manifest = read_manifest(&tmp_dir).unwrap();
        assert_eq!(manifest.snapshots.len(), 1);
        assert_eq!(manifest.snapshots[0].name, "test-snap");

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
}
