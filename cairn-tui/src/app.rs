use std::collections::HashMap;

use cairn_core::{
    CairnClient, ExploreParams, ExploreResult, GraphStatusResult, HistoryParams, HistoryResult,
    NearbyParams, NearbyResult, NodeSummary, SearchResultItem, Topic,
};
use ratatui::widgets::ListState;

use crate::overlays::Overlay;

// ── App state ─────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Browse,
    Filter,
    Search,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Left,
    Right,
}

/// A selectable element in the right pane's detail view. Derived from
/// the cached Detail on every render — not persisted.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum DetailElement {
    Title,
    Tags,
    Summary,
    Block {
        idx: usize,
        block_id: String,
    },
    Edge {
        idx: usize,
        from: String,
        to: String,
        edge_type: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    Detail,
    Neighbors,
    History,
}

impl DetailTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Detail => "detail",
            Self::Neighbors => "neighbors",
            Self::History => "history",
        }
    }

    #[allow(dead_code)]
    pub fn next(self) -> Self {
        match self {
            Self::Detail => Self::Neighbors,
            Self::Neighbors => Self::History,
            Self::History => Self::Detail,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Detail => Self::History,
            Self::Neighbors => Self::Detail,
            Self::History => Self::Neighbors,
        }
    }
}

pub struct Detail {
    pub topic: Topic,
    pub explore: ExploreResult,
}

/// Per-topic caches for the right pane. Cleared on selection change.
#[derive(Default)]
pub struct TopicCaches {
    pub detail: Option<Detail>,
    pub nearby: Option<NearbyResult>,
    pub history: Option<HistoryResult>,
    pub error: Option<String>,
}

pub struct App {
    pub status: GraphStatusResult,
    /// Full topic list, sorted by key. Source of truth for the list pane.
    pub all_topics: Vec<NodeSummary>,
    /// Index into `all_topics` keyed by topic key — used to map search results
    /// back to list rows.
    pub by_key: HashMap<String, usize>,
    /// Indices into `all_topics` currently shown in the list pane.
    pub visible: Vec<usize>,
    pub list_state: ListState,
    pub mode: Mode,
    pub filter: String,
    /// Active server-side FTS query, if any. Mutually exclusive with `filter`.
    pub search_query: String,
    /// True after a search has been confirmed and results are populated.
    pub search_active: bool,
    pub tab: DetailTab,
    pub caches: TopicCaches,

    // ── Edit mode ────────────────────────────────────────────────
    // ── Pane focus ────────────────────────────────────────────────
    /// Which pane has keyboard focus. Tab toggles.
    pub focus: Focus,
    /// Selected element index in the right pane's detail view.
    pub detail_selected: usize,

    // ── Edit mode ────────────────────────────────────────────────
    /// True while this client holds the daemon's editor-session lock.
    /// When set, the header shows `[EDIT MODE]` in red, the footer shows
    /// editing key hints, and Esc exits edit mode (instead of quitting).
    pub edit_mode: bool,
    /// Modal overlay that captures all key input while present.
    pub overlay: Option<Overlay>,
}

impl App {
    pub fn new(status: GraphStatusResult, mut topics: Vec<NodeSummary>) -> Self {
        topics.sort_by(|a, b| a.key.cmp(&b.key));
        let visible: Vec<usize> = (0..topics.len()).collect();
        let by_key = topics
            .iter()
            .enumerate()
            .map(|(i, t)| (t.key.clone(), i))
            .collect();
        let mut list_state = ListState::default();
        if !visible.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            status,
            all_topics: topics,
            by_key,
            visible,
            list_state,
            mode: Mode::Browse,
            filter: String::new(),
            search_query: String::new(),
            search_active: false,
            tab: DetailTab::Detail,
            caches: TopicCaches::default(),
            focus: Focus::Left,
            detail_selected: 0,
            edit_mode: false,
            overlay: None,
        }
    }

    pub fn selected_topic(&self) -> Option<&NodeSummary> {
        let row = self.list_state.selected()?;
        let idx = *self.visible.get(row)?;
        self.all_topics.get(idx)
    }

    pub fn selected_key(&self) -> Option<String> {
        self.selected_topic().map(|t| t.key.clone())
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let len = self.visible.len() as isize;
        let cur = self.list_state.selected().unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(len) as usize;
        self.list_state.select(Some(next));
    }

    pub fn jump_to(&mut self, target: ListJump) {
        if self.visible.is_empty() {
            return;
        }
        let row = match target {
            ListJump::First => 0,
            ListJump::Last => self.visible.len() - 1,
        };
        self.list_state.select(Some(row));
    }

    pub fn reset_visible_to_all(&mut self) {
        self.visible = (0..self.all_topics.len()).collect();
        self.list_state.select(if self.visible.is_empty() {
            None
        } else {
            Some(0)
        });
    }

    pub fn recompute_filter(&mut self) {
        // Filter and search are mutually exclusive — entering filter mode
        // clears any active search.
        self.search_active = false;
        self.search_query.clear();

        let needle = self.filter.trim().to_lowercase();
        self.visible = self
            .all_topics
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                if needle.is_empty() {
                    true
                } else {
                    t.key.to_lowercase().contains(&needle)
                        || t.title.to_lowercase().contains(&needle)
                }
            })
            .map(|(i, _)| i)
            .collect();
        self.list_state.select(if self.visible.is_empty() {
            None
        } else {
            Some(0)
        });
    }

    /// Apply a server FTS result set to the visible list. Search results
    /// arrive ordered by score; we preserve that order. Result keys not
    /// present in `all_topics` (shouldn't happen normally) are skipped.
    pub fn apply_search_results(&mut self, results: &[SearchResultItem]) {
        self.filter.clear();
        self.search_active = true;
        self.visible = results
            .iter()
            .filter_map(|r| self.by_key.get(&r.topic_key).copied())
            .collect();
        self.list_state.select(if self.visible.is_empty() {
            None
        } else {
            Some(0)
        });
    }

    /// Selection changed — drop the per-topic cache and re-fetch whatever
    /// the active tab needs. Sub-ms latency on the local socket means the
    /// blocking await here is fine for v1.
    pub async fn on_selection_changed(&mut self, client: &CairnClient) {
        self.caches = TopicCaches::default();
        self.fetch_active_tab(client).await;
    }

    /// Re-fetch the full topic list and status from the daemon.
    pub async fn refresh(&mut self, client: &CairnClient) {
        match (client.graph_status().await, client.graph_view().await) {
            (Ok(status), Ok(view)) => {
                self.status = status;
                let selected_key = self.selected_key();
                let mut topics = view.topics;
                topics.sort_by(|a, b| a.key.cmp(&b.key));
                self.by_key = topics
                    .iter()
                    .enumerate()
                    .map(|(i, t)| (t.key.clone(), i))
                    .collect();
                self.all_topics = topics;
                self.recompute_filter();
                // Try to re-select the same topic.
                if let Some(key) = selected_key {
                    if let Some(row) = self
                        .visible
                        .iter()
                        .position(|i| self.all_topics.get(*i).map(|t| &t.key) == Some(&key))
                    {
                        self.list_state.select(Some(row));
                    }
                }
                self.caches = TopicCaches::default();
                self.fetch_active_tab(client).await;
            }
            (Err(e), _) | (_, Err(e)) => {
                self.caches.error = Some(format!("refresh: {e}"));
            }
        }
    }

    /// Derive the selectable elements from the cached detail view.
    pub fn detail_elements(&self) -> Vec<DetailElement> {
        let Some(d) = &self.caches.detail else {
            return vec![];
        };
        let mut elems = vec![DetailElement::Title];
        if !d.topic.tags.is_empty() {
            elems.push(DetailElement::Tags);
        }
        if !d.topic.summary.is_empty() {
            elems.push(DetailElement::Summary);
        }
        for (i, block) in d.topic.blocks.iter().enumerate() {
            elems.push(DetailElement::Block {
                idx: i,
                block_id: block.id.clone(),
            });
        }
        for (i, edge) in d.explore.edges.iter().enumerate() {
            elems.push(DetailElement::Edge {
                idx: i,
                from: edge.from.clone(),
                to: edge.to.clone(),
                edge_type: edge.edge_type.clone(),
            });
        }
        elems
    }

    /// Get the currently selected detail element, if any.
    pub fn selected_detail_element(&self) -> Option<DetailElement> {
        let elems = self.detail_elements();
        elems.into_iter().nth(self.detail_selected)
    }

    pub async fn fetch_active_tab(&mut self, client: &CairnClient) {
        let Some(key) = self.selected_key() else {
            return;
        };
        match self.tab {
            DetailTab::Detail => {
                if self.caches.detail.is_some() {
                    return;
                }
                let topic = client.get_topic(&key).await;
                let explore = client
                    .explore(ExploreParams {
                        topic_key: key.clone(),
                        depth: 1,
                        edge_types: Vec::new(),
                    })
                    .await;
                match (topic, explore) {
                    (Ok(topic), Ok(explore)) => {
                        self.caches.detail = Some(Detail { topic, explore });
                        self.caches.error = None;
                    }
                    (Err(e), _) | (_, Err(e)) => {
                        self.caches.detail = None;
                        self.caches.error = Some(e.to_string());
                    }
                }
            }
            DetailTab::Neighbors => {
                if self.caches.nearby.is_some() {
                    return;
                }
                match client
                    .nearby(NearbyParams {
                        topic_key: key,
                        hops: 2,
                    })
                    .await
                {
                    Ok(n) => {
                        self.caches.nearby = Some(n);
                        self.caches.error = None;
                    }
                    Err(e) => {
                        self.caches.nearby = None;
                        self.caches.error = Some(e.to_string());
                    }
                }
            }
            DetailTab::History => {
                if self.caches.history.is_some() {
                    return;
                }
                match client
                    .history(HistoryParams {
                        topic_key: Some(key),
                        limit: 50,
                        session_id: None,
                    })
                    .await
                {
                    Ok(h) => {
                        self.caches.history = Some(h);
                        self.caches.error = None;
                    }
                    Err(e) => {
                        self.caches.history = None;
                        self.caches.error = Some(e.to_string());
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
pub enum ListJump {
    First,
    Last,
}
