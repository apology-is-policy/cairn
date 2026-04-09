use std::collections::{HashMap, HashSet, VecDeque};

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::db::CairnDb;
use crate::error::{CairnError, Result};
use crate::types::*;

// ── Internal helpers ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct FtsRow {
    key: String,
    title: String,
    summary: String,
    score: f64,
}

#[derive(Debug, Deserialize)]
struct TopicSummaryRow {
    key: String,
    title: String,
    summary: String,
}

#[derive(Debug, Deserialize)]
struct EdgeRow {
    from_key: String,
    to_key: String,
    edge_type: String,
    note: String,
}

/// Internal row for edge query — SurrealDB returns `in` and `out` as record refs.
#[derive(Debug, Deserialize)]
struct RawEdgeRow {
    #[serde(rename = "in")]
    in_id: surrealdb::sql::Thing,
    out: surrealdb::sql::Thing,
    note: String,
}

/// Build a lookup table from record ID to topic key.
async fn build_id_key_map(db: &CairnDb) -> Result<HashMap<String, String>> {
    #[derive(Debug, Deserialize)]
    struct IdKeyRow {
        key: String,
    }
    // We need the id too, but can't deserialize it directly in a struct with other fields.
    // Use a separate query for id and key.
    let mut res = db
        .db
        .query("SELECT key FROM topic")
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;
    let rows: Vec<IdKeyRow> = res.take(0).map_err(|e| CairnError::Db(e.to_string()))?;

    // Also get the record IDs
    let mut id_res = db
        .db
        .query("SELECT VALUE id FROM topic")
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;
    let ids: Vec<surrealdb::sql::Thing> =
        id_res.take(0).map_err(|e| CairnError::Db(e.to_string()))?;

    let map: HashMap<String, String> = ids
        .into_iter()
        .zip(rows.into_iter())
        .map(|(id, row)| (id.to_string(), row.key))
        .collect();

    Ok(map)
}

/// Query all edges for a set of topic keys, returning normalized edge info.
async fn edges_for_topics(db: &CairnDb, topic_keys: &[String]) -> Result<Vec<EdgeRow>> {
    if topic_keys.is_empty() {
        return Ok(vec![]);
    }

    // Build ID-to-key lookup
    let id_key_map = build_id_key_map(db).await?;

    // Reverse map: key -> record ID
    let key_id_map: HashMap<&str, &str> = id_key_map
        .iter()
        .map(|(id, key)| (key.as_str(), id.as_str()))
        .collect();

    // Get the record IDs for our target keys
    let target_ids: HashSet<&str> = topic_keys
        .iter()
        .filter_map(|k| key_id_map.get(k.as_str()).copied())
        .collect();

    let mut all_edges = Vec::new();

    for kind in EdgeKind::ALL {
        let table = kind.table_name();
        let query = format!("SELECT in, out, note FROM {table}");

        let mut res = db
            .db
            .query(&query)
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;

        let rows: Vec<RawEdgeRow> = res.take(0).map_err(|e| CairnError::Db(e.to_string()))?;

        for row in rows {
            let in_str = row.in_id.to_string();
            let out_str = row.out.to_string();

            // Only include edges where at least one end is in our target set
            if !target_ids.contains(in_str.as_str()) && !target_ids.contains(out_str.as_str()) {
                continue;
            }

            if let (Some(from_key), Some(to_key)) =
                (id_key_map.get(&in_str), id_key_map.get(&out_str))
            {
                all_edges.push(EdgeRow {
                    from_key: from_key.clone(),
                    to_key: to_key.clone(),
                    edge_type: table.to_string(),
                    note: row.note.clone(),
                });
            }
        }
    }

    Ok(all_edges)
}

/// Get topic summaries for a set of keys.
async fn topic_summaries(db: &CairnDb, keys: &[String]) -> Result<Vec<TopicSummaryRow>> {
    if keys.is_empty() {
        return Ok(vec![]);
    }

    let mut res = db
        .db
        .query("SELECT key, title, summary FROM topic WHERE key IN $keys AND deprecated = false")
        .bind(("keys", keys.to_vec()))
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;

    let rows: Vec<TopicSummaryRow> = res.take(0).map_err(|e| CairnError::Db(e.to_string()))?;
    Ok(rows)
}

// ── Operations ───────────────────────────────────────────────────

/// Full-text search across all topic content.
pub async fn search(db: &CairnDb, params: SearchParams) -> Result<SearchResult> {
    // SurrealDB FTS with BM25 scoring — search title and summary separately,
    // then combine results (SurrealDB requires separate indexes per field)
    let mut res = db
        .db
        .query(
            "SELECT key, title, summary, search::score(1) + search::score(2) AS score
            FROM topic
            WHERE (title @1@ $query OR summary @2@ $query)
                AND deprecated = false
            ORDER BY score DESC
            LIMIT $limit",
        )
        .bind(("query", params.query.clone()))
        .bind(("limit", params.limit))
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;

    let rows: Vec<FtsRow> = res.take(0).map_err(|e| CairnError::Db(e.to_string()))?;
    let total_matches = rows.len();

    let mut results = Vec::new();
    for row in &rows {
        let neighbors = if params.expand {
            // Fetch 1-hop neighbors
            let edges = edges_for_topics(db, std::slice::from_ref(&row.key)).await?;
            let neighbor_keys: Vec<String> = edges
                .iter()
                .map(|e| {
                    if e.from_key == row.key {
                        e.to_key.clone()
                    } else {
                        e.from_key.clone()
                    }
                })
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();

            let summaries = topic_summaries(db, &neighbor_keys).await?;
            let summary_map: HashMap<_, _> =
                summaries.into_iter().map(|s| (s.key.clone(), s)).collect();

            edges
                .iter()
                .filter_map(|e| {
                    let neighbor_key = if e.from_key == row.key {
                        &e.to_key
                    } else {
                        &e.from_key
                    };
                    summary_map.get(neighbor_key).map(|s| NeighborSummary {
                        key: s.key.clone(),
                        edge: e.edge_type.clone(),
                        title: s.title.clone(),
                    })
                })
                .collect()
        } else {
            vec![]
        };

        results.push(SearchResultItem {
            topic_key: row.key.clone(),
            title: row.title.clone(),
            summary: row.summary.clone(),
            score: row.score,
            neighbors,
        });
    }

    Ok(SearchResult {
        results,
        total_matches,
    })
}

/// Given a topic, show all its edges and neighbors up to `depth` hops.
pub async fn explore(db: &CairnDb, params: ExploreParams) -> Result<ExploreResult> {
    let mut visited_keys: HashSet<String> = HashSet::new();
    let mut frontier = vec![params.topic_key.clone()];
    let mut all_edges_out: Vec<EdgeSummary> = Vec::new();

    visited_keys.insert(params.topic_key.clone());

    for _hop in 0..params.depth {
        if frontier.is_empty() {
            break;
        }

        let edges = edges_for_topics(db, &frontier).await?;
        let mut next_frontier = Vec::new();

        for e in &edges {
            // Filter by edge types if specified
            if !params.edge_types.is_empty() {
                if let Some(kind) = EdgeKind::from_table_name(&e.edge_type) {
                    if !params.edge_types.contains(&kind) {
                        continue;
                    }
                }
            }

            all_edges_out.push(EdgeSummary {
                from: e.from_key.clone(),
                to: e.to_key.clone(),
                edge_type: e.edge_type.clone(),
                note: e.note.clone(),
            });

            // Add newly discovered keys to frontier
            for key in [&e.from_key, &e.to_key] {
                if visited_keys.insert(key.clone()) {
                    next_frontier.push(key.clone());
                }
            }
        }

        frontier = next_frontier;
    }

    // Fetch summaries for all visited nodes
    let all_keys: Vec<String> = visited_keys.into_iter().collect();
    let summaries = topic_summaries(db, &all_keys).await?;

    let nodes: Vec<NodeSummary> = summaries
        .into_iter()
        .map(|s| NodeSummary {
            key: s.key,
            title: s.title,
            summary: s.summary,
        })
        .collect();

    Ok(ExploreResult {
        center: params.topic_key,
        nodes,
        edges: all_edges_out,
    })
}

/// Find how two topics are connected through the graph (BFS).
pub async fn path(db: &CairnDb, params: PathParams) -> Result<PathResult> {
    // BFS to find shortest path
    type PathEdge = (String, String, String, String); // (from_key, edge_type, note, to_key)
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, Vec<PathEdge>)> = VecDeque::new();

    visited.insert(params.from.clone());
    queue.push_back((params.from.clone(), vec![]));

    while let Some((current, path_so_far)) = queue.pop_front() {
        if path_so_far.len() >= params.max_depth {
            continue;
        }

        let edges = edges_for_topics(db, std::slice::from_ref(&current)).await?;

        for e in &edges {
            let neighbor = if e.from_key == current {
                &e.to_key
            } else {
                &e.from_key
            };

            if visited.contains(neighbor) {
                continue;
            }
            visited.insert(neighbor.clone());

            let mut new_path = path_so_far.clone();
            new_path.push((
                current.clone(),
                e.edge_type.clone(),
                e.note.clone(),
                neighbor.clone(),
            ));

            if *neighbor == params.to {
                // Found it! Build the PathResult
                let mut steps = Vec::new();
                for (from, edge_type, note, to) in &new_path {
                    if steps.is_empty() {
                        steps.push(PathStep {
                            node: Some(from.clone()),
                            edge: None,
                            note: None,
                        });
                    }
                    steps.push(PathStep {
                        node: None,
                        edge: Some(edge_type.clone()),
                        note: Some(note.clone()),
                    });
                    steps.push(PathStep {
                        node: Some(to.clone()),
                        edge: None,
                        note: None,
                    });
                }

                return Ok(PathResult {
                    found: true,
                    path: steps,
                    depth: new_path.len(),
                });
            }

            queue.push_back((neighbor.clone(), new_path));
        }
    }

    Ok(PathResult {
        found: false,
        path: vec![],
        depth: 0,
    })
}

/// Return all topics within N hops, grouped by edge type.
pub async fn nearby(db: &CairnDb, params: NearbyParams) -> Result<NearbyResult> {
    let mut by_edge_type: HashMap<String, Vec<NearbyEntry>> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut frontier = vec![params.topic_key.clone()];

    visited.insert(params.topic_key.clone());

    for distance in 1..=params.hops {
        if frontier.is_empty() {
            break;
        }

        let edges = edges_for_topics(db, &frontier).await?;
        let mut next_frontier = Vec::new();

        for e in &edges {
            let neighbor_key = if frontier.contains(&e.from_key) {
                &e.to_key
            } else {
                &e.from_key
            };

            if visited.contains(neighbor_key) {
                continue;
            }
            visited.insert(neighbor_key.clone());
            next_frontier.push(neighbor_key.clone());

            // Get the topic title
            let summaries = topic_summaries(db, std::slice::from_ref(neighbor_key)).await?;
            let title = summaries
                .first()
                .map(|s| s.title.clone())
                .unwrap_or_default();

            by_edge_type
                .entry(e.edge_type.clone())
                .or_default()
                .push(NearbyEntry {
                    key: neighbor_key.clone(),
                    title,
                    distance,
                });
        }

        frontier = next_frontier;
    }

    let total_nodes = visited.len() - 1; // exclude center

    Ok(NearbyResult {
        center: params.topic_key,
        by_edge_type,
        total_nodes,
    })
}

/// Return all topics and edges for graph visualization.
pub async fn graph_view(db: &CairnDb) -> Result<GraphViewResult> {
    let mut res = db
        .db
        .query("SELECT key, title, summary FROM topic WHERE deprecated = false ORDER BY key ASC")
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;
    let rows: Vec<TopicSummaryRow> = res.take(0).map_err(|e| CairnError::Db(e.to_string()))?;

    let topics: Vec<NodeSummary> = rows
        .into_iter()
        .map(|r| NodeSummary {
            key: r.key,
            title: r.title,
            summary: r.summary,
        })
        .collect();

    let id_key_map = build_id_key_map(db).await?;
    let mut edges = Vec::new();

    for kind in EdgeKind::ALL {
        let table = kind.table_name();
        let query = format!("SELECT in, out, note FROM {table}");
        let mut res = db
            .db
            .query(&query)
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;
        let raw_rows: Vec<RawEdgeRow> = res.take(0).map_err(|e| CairnError::Db(e.to_string()))?;

        for row in raw_rows {
            if let (Some(from_key), Some(to_key)) = (
                id_key_map.get(&row.in_id.to_string()),
                id_key_map.get(&row.out.to_string()),
            ) {
                edges.push(EdgeSummary {
                    from: from_key.clone(),
                    to: to_key.clone(),
                    edge_type: table.to_string(),
                    note: row.note,
                });
            }
        }
    }

    Ok(GraphViewResult { topics, edges })
}

/// Graph overview statistics.
pub async fn stats(db: &CairnDb) -> Result<StatsResult> {
    // Topic counts
    #[derive(Deserialize)]
    struct CountRow {
        count: usize,
    }

    let mut total_res = db
        .db
        .query("SELECT count() AS count FROM topic GROUP ALL")
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;
    let total: usize = total_res
        .take::<Option<CountRow>>(0)
        .map_err(|e| CairnError::Db(e.to_string()))?
        .map(|r| r.count)
        .unwrap_or(0);

    let mut dep_res = db
        .db
        .query("SELECT count() AS count FROM topic WHERE deprecated = true GROUP ALL")
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;
    let deprecated: usize = dep_res
        .take::<Option<CountRow>>(0)
        .map_err(|e| CairnError::Db(e.to_string()))?
        .map(|r| r.count)
        .unwrap_or(0);

    let mut stale_res = db
        .db
        .query(
            "SELECT count() AS count FROM topic
            WHERE updated_at < time::now() - 90d AND deprecated = false
            GROUP ALL",
        )
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;
    let stale_90d: usize = stale_res
        .take::<Option<CountRow>>(0)
        .map_err(|e| CairnError::Db(e.to_string()))?
        .map(|r| r.count)
        .unwrap_or(0);

    let active = total - deprecated;

    // Edge counts by type
    let mut edge_total = 0usize;
    let mut by_type = HashMap::new();

    for kind in EdgeKind::ALL {
        let table = kind.table_name();
        let query = format!("SELECT count() AS count FROM {table} GROUP ALL");
        let mut res = db
            .db
            .query(&query)
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;
        let count: usize = res
            .take::<Option<CountRow>>(0)
            .map_err(|e| CairnError::Db(e.to_string()))?
            .map(|r| r.count)
            .unwrap_or(0);
        edge_total += count;
        by_type.insert(table.to_string(), count);
    }

    // Most connected topics (by total edge count across all edge types)
    // This is expensive for large graphs; we'll do a simple approach
    #[derive(Deserialize)]
    struct TopicKeyRow {
        key: String,
        title: String,
    }

    let mut topic_res = db
        .db
        .query("SELECT key, title FROM topic WHERE deprecated = false LIMIT 100")
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;
    let all_topics: Vec<TopicKeyRow> = topic_res
        .take(0)
        .map_err(|e| CairnError::Db(e.to_string()))?;

    // Recently updated
    #[derive(Deserialize)]
    struct RecentRow {
        key: String,
        title: String,
        updated_at: DateTime<Utc>,
    }

    let mut recent_res = db
        .db
        .query(
            "SELECT key, title, updated_at FROM topic
            WHERE deprecated = false
            ORDER BY updated_at DESC LIMIT 5",
        )
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;
    let recent: Vec<RecentRow> = recent_res
        .take(0)
        .map_err(|e| CairnError::Db(e.to_string()))?;

    let recently_updated: Vec<TopicRank> = recent
        .into_iter()
        .map(|r| TopicRank {
            key: r.key,
            title: r.title,
            edge_count: None,
            updated_at: Some(r.updated_at),
        })
        .collect();

    // Oldest untouched
    let mut old_res = db
        .db
        .query(
            "SELECT key, title, updated_at FROM topic
            WHERE deprecated = false
            ORDER BY updated_at ASC LIMIT 5",
        )
        .await
        .map_err(|e| CairnError::Db(e.to_string()))?;
    let old: Vec<RecentRow> = old_res.take(0).map_err(|e| CairnError::Db(e.to_string()))?;

    let oldest_untouched: Vec<TopicRank> = old
        .into_iter()
        .map(|r| TopicRank {
            key: r.key,
            title: r.title,
            edge_count: None,
            updated_at: Some(r.updated_at),
        })
        .collect();

    // For most connected, we'll use a simple count of edges
    // For each topic key in our recent list, count edges
    let mut most_connected: Vec<TopicRank> = Vec::new();
    for t in all_topics.iter().take(20) {
        let edges = edges_for_topics(db, std::slice::from_ref(&t.key)).await?;
        most_connected.push(TopicRank {
            key: t.key.clone(),
            title: t.title.clone(),
            edge_count: Some(edges.len()),
            updated_at: None,
        });
    }
    most_connected.sort_by(|a, b| b.edge_count.cmp(&a.edge_count));
    most_connected.truncate(5);

    Ok(StatsResult {
        topics: TopicStats {
            total,
            active,
            deprecated,
            stale_90d,
        },
        edges: EdgeStats {
            total: edge_total,
            by_type,
        },
        most_connected,
        recently_updated,
        oldest_untouched,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops;

    async fn test_db() -> CairnDb {
        CairnDb::open_memory().await.unwrap()
    }

    /// Create a small test graph:
    /// billing-retry --depends_on--> event-bus
    /// billing-retry --gotcha--> idempotency-keys
    /// event-bus --see_also--> monitoring
    /// billing-retry --war_story--> march-incident
    async fn setup_test_graph(db: &CairnDb) {
        let topics = vec![
            (
                "billing-retry",
                "Payment retry mechanism",
                "Handles payment retries with exponential backoff",
            ),
            (
                "event-bus",
                "Event bus core",
                "Central event bus for async messaging",
            ),
            (
                "idempotency-keys",
                "Idempotency key handling",
                "Prevents duplicate payment processing",
            ),
            (
                "monitoring",
                "Monitoring and alerts",
                "Grafana dashboards and PagerDuty integration",
            ),
            (
                "march-incident",
                "March DLQ incident",
                "DLQ overflow caused lost payments",
            ),
        ];

        for (key, title, content) in topics {
            ops::learn(
                db,
                LearnParams {
                    topic_key: key.into(),
                    title: Some(title.into()),
                    summary: Some(content.into()),
                    content: content.into(),
                    voice: None,
                    tags: vec![],
                    position: Position::End,
                },
            )
            .await
            .unwrap();
        }

        let edges = vec![
            (
                "billing-retry",
                "event-bus",
                EdgeKind::DependsOn,
                "Retry logic reads event bus serialization format",
            ),
            (
                "billing-retry",
                "idempotency-keys",
                EdgeKind::Gotcha,
                "Must check idempotency before retrying",
            ),
            (
                "event-bus",
                "monitoring",
                EdgeKind::SeeAlso,
                "Event bus metrics feed into monitoring",
            ),
            (
                "billing-retry",
                "march-incident",
                EdgeKind::WarStory,
                "DLQ overflow caused by missing retry cap",
            ),
        ];

        for (from, to, kind, note) in edges {
            ops::connect(
                db,
                ConnectParams {
                    from: from.into(),
                    to: to.into(),
                    edge_type: kind,
                    note: note.into(),
                    severity: if kind == EdgeKind::Gotcha {
                        Some(Severity::High)
                    } else {
                        None
                    },
                },
            )
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn test_search_fts() {
        let db = test_db().await;
        setup_test_graph(&db).await;

        let result = search(
            &db,
            SearchParams {
                query: "retry".into(),
                expand: false,
                limit: 10,
            },
        )
        .await
        .unwrap();

        assert!(!result.results.is_empty());
        assert_eq!(result.results[0].topic_key, "billing-retry");
    }

    #[tokio::test]
    async fn test_search_with_expand() {
        let db = test_db().await;
        setup_test_graph(&db).await;

        let result = search(
            &db,
            SearchParams {
                query: "retry".into(),
                expand: true,
                limit: 10,
            },
        )
        .await
        .unwrap();

        assert!(!result.results.is_empty());
        // billing-retry should have neighbors
        let billing = &result.results[0];
        assert!(!billing.neighbors.is_empty());
    }

    #[tokio::test]
    async fn test_explore() {
        let db = test_db().await;
        setup_test_graph(&db).await;

        let result = explore(
            &db,
            ExploreParams {
                topic_key: "billing-retry".into(),
                depth: 1,
                edge_types: vec![],
            },
        )
        .await
        .unwrap();

        assert_eq!(result.center, "billing-retry");
        // billing-retry has 3 direct edges (depends_on, gotcha, war_story)
        assert!(result.edges.len() >= 3);
        // Should have discovered at least the direct neighbors
        assert!(result.nodes.len() >= 3);
    }

    #[tokio::test]
    async fn test_explore_filtered() {
        let db = test_db().await;
        setup_test_graph(&db).await;

        let result = explore(
            &db,
            ExploreParams {
                topic_key: "billing-retry".into(),
                depth: 1,
                edge_types: vec![EdgeKind::DependsOn],
            },
        )
        .await
        .unwrap();

        // Only depends_on edges
        assert!(result.edges.iter().all(|e| e.edge_type == "depends_on"));
    }

    #[tokio::test]
    async fn test_path_direct() {
        let db = test_db().await;
        setup_test_graph(&db).await;

        let result = path(
            &db,
            PathParams {
                from: "billing-retry".into(),
                to: "event-bus".into(),
                max_depth: 5,
            },
        )
        .await
        .unwrap();

        assert!(result.found);
        assert_eq!(result.depth, 1);
    }

    #[tokio::test]
    async fn test_path_indirect() {
        let db = test_db().await;
        setup_test_graph(&db).await;

        // billing-retry -> event-bus -> monitoring (2 hops)
        let result = path(
            &db,
            PathParams {
                from: "billing-retry".into(),
                to: "monitoring".into(),
                max_depth: 5,
            },
        )
        .await
        .unwrap();

        assert!(result.found);
        assert_eq!(result.depth, 2);
    }

    #[tokio::test]
    async fn test_path_not_found() {
        let db = test_db().await;
        setup_test_graph(&db).await;

        // Create an isolated topic
        ops::learn(
            &db,
            LearnParams {
                topic_key: "isolated".into(),
                title: Some("Isolated topic".into()),
                summary: None,
                content: "No connections".into(),
                voice: None,
                tags: vec![],
                position: Position::End,
            },
        )
        .await
        .unwrap();

        let result = path(
            &db,
            PathParams {
                from: "billing-retry".into(),
                to: "isolated".into(),
                max_depth: 5,
            },
        )
        .await
        .unwrap();

        assert!(!result.found);
    }

    #[tokio::test]
    async fn test_nearby() {
        let db = test_db().await;
        setup_test_graph(&db).await;

        let result = nearby(
            &db,
            NearbyParams {
                topic_key: "billing-retry".into(),
                hops: 2,
            },
        )
        .await
        .unwrap();

        assert_eq!(result.center, "billing-retry");
        assert!(result.total_nodes >= 3);
        // Should have entries in multiple edge types
        assert!(!result.by_edge_type.is_empty());
    }

    #[tokio::test]
    async fn test_stats() {
        let db = test_db().await;
        setup_test_graph(&db).await;

        let result = stats(&db).await.unwrap();

        assert_eq!(result.topics.total, 5);
        assert_eq!(result.topics.active, 5);
        assert_eq!(result.topics.deprecated, 0);
        assert_eq!(result.edges.total, 4);
        assert!(result.edges.by_type.contains_key("depends_on"));
    }

    #[tokio::test]
    async fn test_stats_with_deprecated() {
        let db = test_db().await;
        setup_test_graph(&db).await;

        ops::forget(
            &db,
            ForgetParams {
                topic_key: "march-incident".into(),
                reason: "resolved".into(),
            },
        )
        .await
        .unwrap();

        let result = stats(&db).await.unwrap();

        assert_eq!(result.topics.total, 5);
        assert_eq!(result.topics.active, 4);
        assert_eq!(result.topics.deprecated, 1);
    }
}
