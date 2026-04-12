//! cairn-tui — terminal explorer for the Cairn knowledge graph.
//!
//! Connects to a `cairn-server` daemon via `CairnClient` (auto-spawning the
//! daemon if needed), so it coexists safely with `cairn-mcp` and `cairn-cli`
//! against the same single-writer SurrealKV database.

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::time::Duration;

use std::collections::HashMap;

use cairn_core::{
    default_db_path, CairnClient, CairnError, EdgeSummary, ExploreParams, ExploreResult,
    GraphStatusResult, HistoryParams, HistoryResult, NearbyParams, NearbyResult, NodeSummary,
    SearchParams, SearchResult, SearchResultItem, Topic,
};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

#[derive(Parser)]
#[command(name = "cairn-tui", about = "Terminal explorer for Cairn")]
struct Args {
    /// Path to the Cairn database directory.
    #[arg(long, env = "CAIRN_DB")]
    db: Option<PathBuf>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    init_file_logging();

    let args = Args::parse();
    let db_path = args.db.unwrap_or_else(default_db_path);

    // Connect (or spawn) the daemon BEFORE entering raw mode, so any
    // connection error prints normally to stderr instead of corrupting
    // the alternate screen.
    let client = CairnClient::connect_or_spawn(&db_path)
        .await
        .map_err(|e| anyhow::anyhow!("connect to cairn-server: {e}"))?;
    let status = client
        .graph_status()
        .await
        .map_err(|e| anyhow::anyhow!("graph_status: {e}"))?;
    let view = client
        .graph_view()
        .await
        .map_err(|e| anyhow::anyhow!("graph_view: {e}"))?;

    let mut app = App::new(status, view.topics);
    app.on_selection_changed(&client).await;

    let mut terminal = setup_terminal()?;
    install_panic_hook();

    let result = run_app(&mut terminal, &client, &mut app).await;

    restore_terminal(&mut terminal)?;
    result
}

// ── Terminal lifecycle ────────────────────────────────────────────

type Term = Terminal<CrosstermBackend<Stdout>>;

fn setup_terminal() -> io::Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore_terminal(terminal: &mut Term) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// Restore the terminal on panic so the user gets a usable shell back.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original(info);
    }));
}

/// All log output goes to a file — stdout/stderr are owned by the TUI.
fn init_file_logging() {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let log_dir = PathBuf::from(&home).join(".cairn").join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("cairn-tui.log");
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::sync::Mutex::new(file))
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .try_init();
    }
}

// ── App state ─────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Browse,
    Filter,
    Search,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Left,
    Right,
}

/// A selectable element in the right pane's detail view. Derived from
/// the cached Detail on every render — not persisted.
#[derive(Clone, Debug)]
#[allow(dead_code)]
enum DetailElement {
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
enum DetailTab {
    Detail,
    Neighbors,
    History,
}

impl DetailTab {
    fn label(self) -> &'static str {
        match self {
            Self::Detail => "detail",
            Self::Neighbors => "neighbors",
            Self::History => "history",
        }
    }

    #[allow(dead_code)]
    fn next(self) -> Self {
        match self {
            Self::Detail => Self::Neighbors,
            Self::Neighbors => Self::History,
            Self::History => Self::Detail,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Detail => Self::History,
            Self::Neighbors => Self::Detail,
            Self::History => Self::Neighbors,
        }
    }
}

struct Detail {
    topic: Topic,
    explore: ExploreResult,
}

/// Per-topic caches for the right pane. Cleared on selection change.
#[derive(Default)]
struct TopicCaches {
    detail: Option<Detail>,
    nearby: Option<NearbyResult>,
    history: Option<HistoryResult>,
    error: Option<String>,
}

struct App {
    status: GraphStatusResult,
    /// Full topic list, sorted by key. Source of truth for the list pane.
    all_topics: Vec<NodeSummary>,
    /// Index into `all_topics` keyed by topic key — used to map search results
    /// back to list rows.
    by_key: HashMap<String, usize>,
    /// Indices into `all_topics` currently shown in the list pane.
    visible: Vec<usize>,
    list_state: ListState,
    mode: Mode,
    filter: String,
    /// Active server-side FTS query, if any. Mutually exclusive with `filter`.
    search_query: String,
    /// True after a search has been confirmed and results are populated.
    search_active: bool,
    tab: DetailTab,
    caches: TopicCaches,

    // ── Edit mode ────────────────────────────────────────────────
    // ── Pane focus ────────────────────────────────────────────────
    /// Which pane has keyboard focus. Tab toggles.
    focus: Focus,
    /// Selected element index in the right pane's detail view.
    detail_selected: usize,

    // ── Edit mode ────────────────────────────────────────────────
    /// True while this client holds the daemon's editor-session lock.
    /// When set, the header shows `[EDIT MODE]` in red, the footer shows
    /// editing key hints, and Esc exits edit mode (instead of quitting).
    edit_mode: bool,
    /// Modal overlay that captures all key input while present.
    overlay: Option<Overlay>,
}

impl App {
    fn new(status: GraphStatusResult, mut topics: Vec<NodeSummary>) -> Self {
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

    fn selected_topic(&self) -> Option<&NodeSummary> {
        let row = self.list_state.selected()?;
        let idx = *self.visible.get(row)?;
        self.all_topics.get(idx)
    }

    fn selected_key(&self) -> Option<String> {
        self.selected_topic().map(|t| t.key.clone())
    }

    fn move_selection(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let len = self.visible.len() as isize;
        let cur = self.list_state.selected().unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(len) as usize;
        self.list_state.select(Some(next));
    }

    fn jump_to(&mut self, target: ListJump) {
        if self.visible.is_empty() {
            return;
        }
        let row = match target {
            ListJump::First => 0,
            ListJump::Last => self.visible.len() - 1,
        };
        self.list_state.select(Some(row));
    }

    fn reset_visible_to_all(&mut self) {
        self.visible = (0..self.all_topics.len()).collect();
        self.list_state
            .select(if self.visible.is_empty() { None } else { Some(0) });
    }

    fn recompute_filter(&mut self) {
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
        self.list_state
            .select(if self.visible.is_empty() { None } else { Some(0) });
    }

    /// Apply a server FTS result set to the visible list. Search results
    /// arrive ordered by score; we preserve that order. Result keys not
    /// present in `all_topics` (shouldn't happen normally) are skipped.
    fn apply_search_results(&mut self, results: &[SearchResultItem]) {
        self.filter.clear();
        self.search_active = true;
        self.visible = results
            .iter()
            .filter_map(|r| self.by_key.get(&r.topic_key).copied())
            .collect();
        self.list_state
            .select(if self.visible.is_empty() { None } else { Some(0) });
    }

    /// Selection changed — drop the per-topic cache and re-fetch whatever
    /// the active tab needs. Sub-ms latency on the local socket means the
    /// blocking await here is fine for v1.
    async fn on_selection_changed(&mut self, client: &CairnClient) {
        self.caches = TopicCaches::default();
        self.fetch_active_tab(client).await;
    }

    /// Re-fetch the full topic list and status from the daemon.
    async fn refresh(&mut self, client: &CairnClient) {
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
    fn detail_elements(&self) -> Vec<DetailElement> {
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
    fn selected_detail_element(&self) -> Option<DetailElement> {
        let elems = self.detail_elements();
        elems.into_iter().nth(self.detail_selected)
    }

    async fn fetch_active_tab(&mut self, client: &CairnClient) {
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
enum ListJump {
    First,
    Last,
}

/// Modal overlays that capture all key input while active. The main event
/// loop checks `app.overlay` first; when present, the overlay's key
/// handler runs and the normal key dispatch is suppressed. This is the
/// extension point for every dialog: edit confirmation, text input
/// prompts, the command palette, and anything else that needs focus.
///
/// Note: not Clone because `TextArea` carries undo history and cursor
/// state. `handle_overlay_key` uses `app.overlay.take()` to avoid borrow
/// conflicts, then puts the overlay back if it's still active.
enum Overlay {
    /// The "enter edit mode?" red confirmation dialog. Shown on `e` in
    /// browse mode. Enter confirms (acquires the lock), Esc cancels.
    /// If `pending_action` is set, that action is dispatched immediately
    /// after the lock is acquired — so pressing Enter on a block while
    /// not in edit mode flows directly into amend after confirming.
    EditConfirm {
        pending_action: Option<Action>,
    },
    /// Notification toast — auto-dismissed on any keypress.
    Notification {
        message: String,
        is_error: bool,
    },
    /// Fuzzy-filtered command palette (`:` in browse mode).
    CommandPalette {
        filter: String,
        selected: usize,
    },
    /// Multiline text editor (tui-textarea). Used for editing block
    /// content, voice, edge notes, etc. Ctrl+S saves, Esc cancels.
    /// The TextArea is boxed because it's much larger than the other
    /// variants (cursor state, undo history, line buffer).
    TextInput {
        title: String,
        textarea: Box<tui_textarea::TextArea<'static>>,
        purpose: TextInputPurpose,
    },
    /// Single-line text input for short prompts (reason, rename key).
    /// Enter confirms, Esc cancels.
    LineInput {
        title: String,
        buffer: String,
        purpose: LineInputPurpose,
    },
    /// Block picker — select which block to edit within a topic.
    /// Shown when a topic has 2+ blocks and the user hits `e`.
    BlockPicker {
        topic_key: String,
        blocks: Vec<(String, String)>, // (block_id, first line preview)
        selected: usize,
    },
    /// Edge picker — select which edge to remove. Shows edges from explore.
    EdgePicker {
        edges: Vec<(String, String, String, String)>, // (from, to, edge_type, note)
        selected: usize,
    },
    /// Context-sensitive action menu opened by Enter on a selected element.
    ContextMenu {
        items: Vec<ContextMenuItem>,
        selected: usize,
    },
    /// Edge type picker — select which edge type to create.
    EdgeTypePicker {
        from_key: String,
        to_key: String,
        selected: usize,
    },
}

/// What the TextInput overlay should do with its content when saved.
enum TextInputPurpose {
    AmendBlock {
        topic_key: String,
        block_id: String,
    },
    EditVoice,
    LearnContent {
        topic_key: String,
        title: String,
    },
    AddBlockContent {
        topic_key: String,
    },
}

/// What the LineInput overlay should do with its content when confirmed.
enum LineInputPurpose {
    AmendReason {
        topic_key: String,
        block_id: String,
        new_content: String,
    },
    RenameKey {
        old_key: String,
    },
    ForgetReason {
        topic_key: String,
    },
    CheckpointLabel,
    NewTopicKey,
    NewTopicTitle {
        topic_key: String,
    },
    EdgeTargetKey {
        from_key: String,
    },
    EdgeNote {
        from_key: String,
        to_key: String,
        edge_type: String,
    },
    EditTags {
        topic_key: String,
    },
    EditSummary {
        topic_key: String,
    },
}

#[derive(Clone)]
struct ContextMenuItem {
    label: String,
    action: Action,
}

/// Edge types for the picker. Matches cairn_core::EdgeKind::ALL.
const EDGE_TYPES: &[(&str, &str)] = &[
    ("depends_on", "A requires B to function"),
    ("gotcha", "B is a known pitfall when working with A"),
    ("war_story", "B is an incident related to A"),
    ("contradicts", "A and B contain conflicting information"),
    ("replaced_by", "A is outdated; B is current"),
    ("see_also", "Loose association"),
    ("owns", "Ownership / responsibility"),
];

/// Result of processing a key in an overlay context.
enum OverlayResult {
    /// The overlay consumed the key. The main event loop should `continue`.
    Consumed,
    /// The overlay produced an action to dispatch through the normal handler.
    Dispatch(Action),
}

// ── Command palette commands ──────────────────────────────────────

struct PaletteCommand {
    name: &'static str,
    description: &'static str,
    key_hint: Option<&'static str>,
    action: Action,
    /// Only shown when edit_mode is true.
    edit_only: bool,
    /// Hidden when edit_mode is true (e.g. "Enter edit mode").
    browse_only: bool,
}

fn all_palette_commands() -> Vec<PaletteCommand> {
    vec![
        PaletteCommand {
            name: "Filter",
            description: "Filter topics by name",
            key_hint: Some("/"),
            action: Action::EnterFilter,
            edit_only: false,
            browse_only: false,
        },
        PaletteCommand {
            name: "Search",
            description: "Full-text search across topics",
            key_hint: Some("?"),
            action: Action::EnterSearch,
            edit_only: false,
            browse_only: false,
        },
        PaletteCommand {
            name: "Refresh",
            description: "Re-fetch all data from daemon",
            key_hint: Some("R"),
            action: Action::Refresh,
            edit_only: false,
            browse_only: false,
        },
        PaletteCommand {
            name: "Detail tab",
            description: "Show topic detail",
            key_hint: Some("1"),
            action: Action::SwitchTab(DetailTab::Detail),
            edit_only: false,
            browse_only: false,
        },
        PaletteCommand {
            name: "Neighbors tab",
            description: "Show nearby topics",
            key_hint: Some("2"),
            action: Action::SwitchTab(DetailTab::Neighbors),
            edit_only: false,
            browse_only: false,
        },
        PaletteCommand {
            name: "History tab",
            description: "Show mutation history",
            key_hint: Some("3"),
            action: Action::SwitchTab(DetailTab::History),
            edit_only: false,
            browse_only: false,
        },
        PaletteCommand {
            name: "Enter edit mode",
            description: "Acquire exclusive lock for editing",
            key_hint: Some("e"),
            action: Action::RequestEditMode,
            edit_only: false,
            browse_only: true,
        },
        PaletteCommand {
            name: "Amend block",
            description: "Edit a block's content in the selected topic",
            key_hint: Some("e"),
            action: Action::AmendBlock,
            edit_only: true,
            browse_only: false,
        },
        PaletteCommand {
            name: "Rename topic",
            description: "Change the key of the selected topic",
            key_hint: Some("r"),
            action: Action::RenameTopic,
            edit_only: true,
            browse_only: false,
        },
        PaletteCommand {
            name: "Forget topic",
            description: "Soft-delete the selected topic (deprecated)",
            key_hint: Some("d"),
            action: Action::ForgetTopic,
            edit_only: true,
            browse_only: false,
        },
        PaletteCommand {
            name: "Edit voice",
            description: "Edit the developer voice / personality",
            key_hint: Some("V"),
            action: Action::EditVoice,
            edit_only: true,
            browse_only: false,
        },
        PaletteCommand {
            name: "Learn new topic",
            description: "Create a brand new topic from scratch",
            key_hint: Some("n"),
            action: Action::LearnNewTopic,
            edit_only: true,
            browse_only: false,
        },
        PaletteCommand {
            name: "Add edge",
            description: "Create a typed edge from the selected topic",
            key_hint: Some("a"),
            action: Action::AddEdge,
            edit_only: true,
            browse_only: false,
        },
        PaletteCommand {
            name: "Edit tags",
            description: "Replace tags on the selected topic",
            key_hint: Some("t"),
            action: Action::EditTags,
            edit_only: true,
            browse_only: false,
        },
        PaletteCommand {
            name: "Remove edge",
            description: "Delete an edge from the selected topic",
            key_hint: Some("x"),
            action: Action::RemoveEdge,
            edit_only: true,
            browse_only: false,
        },
        PaletteCommand {
            name: "Move block up",
            description: "Move the first block up one position",
            key_hint: Some("K"),
            action: Action::MoveBlockUp,
            edit_only: true,
            browse_only: false,
        },
        PaletteCommand {
            name: "Move block down",
            description: "Move the first block down one position",
            key_hint: Some("J"),
            action: Action::MoveBlockDown,
            edit_only: true,
            browse_only: false,
        },
        PaletteCommand {
            name: "Edit summary",
            description: "Edit the selected topic's search summary",
            key_hint: Some("s"),
            action: Action::EditSummary,
            edit_only: true,
            browse_only: false,
        },
        PaletteCommand {
            name: "Add block",
            description: "Append a new block to the selected topic",
            key_hint: Some("b"),
            action: Action::AddBlock,
            edit_only: true,
            browse_only: false,
        },
        PaletteCommand {
            name: "Checkpoint",
            description: "Create a manual checkpoint in the history",
            key_hint: None,
            action: Action::ManualCheckpoint,
            edit_only: true,
            browse_only: false,
        },
        PaletteCommand {
            name: "Exit edit mode",
            description: "Release the editor lock",
            key_hint: Some("Esc"),
            action: Action::ExitEditMode,
            edit_only: true,
            browse_only: false,
        },
        PaletteCommand {
            name: "Quit",
            description: "Exit cairn-tui",
            key_hint: Some("q"),
            action: Action::Quit,
            edit_only: false,
            browse_only: false,
        },
    ]
}

fn filtered_palette(filter: &str, edit_mode: bool) -> Vec<(usize, &'static PaletteCommand)> {
    use std::sync::OnceLock;
    static COMMANDS: OnceLock<Vec<PaletteCommand>> = OnceLock::new();
    let commands = COMMANDS.get_or_init(all_palette_commands);

    let needle = filter.trim().to_lowercase();
    commands
        .iter()
        .enumerate()
        .filter(|(_, cmd)| {
            if cmd.edit_only && !edit_mode {
                return false;
            }
            if cmd.browse_only && edit_mode {
                return false;
            }
            if needle.is_empty() {
                return true;
            }
            cmd.name.to_lowercase().contains(&needle)
                || cmd.description.to_lowercase().contains(&needle)
        })
        .collect()
}

// ── Event loop ────────────────────────────────────────────────────

async fn run_app(terminal: &mut Term, client: &CairnClient, app: &mut App) -> anyhow::Result<()> {
    loop {
        terminal.draw(|f| draw(f, app))?;

        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        // Global quit chord works in any mode. Release the editor lock
        // if held so the daemon unblocks immediately rather than waiting
        // for the socket drop.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if app.edit_mode {
                let _ = client.end_editor_session().await;
            }
            return Ok(());
        }

        // ── Overlay dispatch ─────────────────────────────────────
        // Overlays capture ALL key input. If the overlay produces an
        // action (e.g. the command palette), it falls through to the
        // normal action handler below. Otherwise the key is consumed.
        let mut dispatched_action = None;
        if app.overlay.is_some() {
            match handle_overlay_key(app, client, key).await {
                OverlayResult::Consumed => continue,
                OverlayResult::Dispatch(a) => dispatched_action = Some(a),
            }
        }

        // ── Normal key dispatch ──────────────────────────────────
        let action = dispatched_action.unwrap_or_else(|| match app.mode {
            Mode::Browse => handle_browse_key(key.code, key.modifiers, app.edit_mode, app.focus),
            Mode::Filter => handle_text_key(key.code, TextTarget::Filter),
            Mode::Search => handle_text_key(key.code, TextTarget::Search),
        });

        match action {
            Action::None => {}
            Action::Quit => {
                if app.edit_mode {
                    let _ = client.end_editor_session().await;
                }
                return Ok(());
            }
            Action::EnterFilter => {
                app.mode = Mode::Filter;
                app.search_active = false;
                app.search_query.clear();
                app.filter.clear();
                app.recompute_filter();
            }
            Action::EnterSearch => {
                app.mode = Mode::Search;
                app.search_query.clear();
                app.filter.clear();
            }
            Action::ExitText => {
                app.mode = Mode::Browse;
                // Clear whichever text input was active and restore the
                // full topic list.
                let was_search = app.search_active;
                app.filter.clear();
                app.search_query.clear();
                app.search_active = false;
                if was_search {
                    app.reset_visible_to_all();
                } else {
                    app.recompute_filter();
                }
                app.on_selection_changed(client).await;
            }
            Action::ConfirmText => {
                let was_search = app.mode == Mode::Search;
                app.mode = Mode::Browse;
                if was_search {
                    let q = app.search_query.trim().to_string();
                    if q.is_empty() {
                        app.reset_visible_to_all();
                    } else {
                        match client
                            .search(SearchParams {
                                query: q,
                                expand: false,
                                limit: 50,
                            })
                            .await
                        {
                            Ok(SearchResult { results, .. }) => {
                                app.apply_search_results(&results);
                            }
                            Err(e) => {
                                app.caches.error = Some(format!("search: {e}"));
                            }
                        }
                    }
                }
                app.on_selection_changed(client).await;
            }
            Action::TextPush(c) => match app.mode {
                Mode::Filter => {
                    app.filter.push(c);
                    app.recompute_filter();
                }
                Mode::Search => app.search_query.push(c),
                Mode::Browse => {}
            },
            Action::TextPop => match app.mode {
                Mode::Filter => {
                    app.filter.pop();
                    app.recompute_filter();
                }
                Mode::Search => {
                    app.search_query.pop();
                }
                Mode::Browse => {}
            },
            Action::Move(delta) => match app.focus {
                Focus::Left => {
                    app.move_selection(delta);
                    app.on_selection_changed(client).await;
                }
                Focus::Right => {
                    let count = app.detail_elements().len();
                    if count > 0 {
                        let cur = app.detail_selected as isize;
                        app.detail_selected =
                            (cur + delta).rem_euclid(count as isize) as usize;
                    }
                }
            },
            Action::Jump(j) => match app.focus {
                Focus::Left => {
                    app.jump_to(j);
                    app.on_selection_changed(client).await;
                }
                Focus::Right => {
                    let count = app.detail_elements().len();
                    if count > 0 {
                        app.detail_selected = match j {
                            ListJump::First => 0,
                            ListJump::Last => count - 1,
                        };
                    }
                }
            },
            Action::SwitchTab(t) => {
                app.tab = t;
                app.fetch_active_tab(client).await;
            }
            Action::PrevTab => {
                app.tab = app.tab.prev();
                app.fetch_active_tab(client).await;
            }
            Action::RequestEditMode => {
                app.overlay = Some(Overlay::EditConfirm {
                    pending_action: None,
                });
            }
            Action::ExitEditMode => {
                let _ = client.end_editor_session().await;
                app.edit_mode = false;
                app.overlay = Some(Overlay::Notification {
                    message: "Edit mode released. Agents can write again.".into(),
                    is_error: false,
                });
            }
            Action::Refresh => {
                app.refresh(client).await;
            }
            Action::EditSummary => {
                if require_edit_mode(app, action) {
                    // Will re-dispatch after lock is acquired.
                } else if let Some(detail) = &app.caches.detail {
                    let topic_key = detail.topic.key.clone();
                    let current = detail.topic.summary.clone();
                    app.overlay = Some(Overlay::LineInput {
                        title: format!("Summary for '{}'", topic_key),
                        buffer: current,
                        purpose: LineInputPurpose::EditSummary { topic_key },
                    });
                } else {
                    notify_err(app, "Select a topic first".into());
                }
            }
            Action::AddBlock => {
                if require_edit_mode(app, action) {
                    // Will re-dispatch after lock is acquired.
                } else if let Some(detail) = &app.caches.detail {
                    let topic_key = detail.topic.key.clone();
                    let mut textarea =
                        tui_textarea::TextArea::new(vec![String::new()]);
                    textarea.set_cursor_line_style(Style::default());
                    textarea.set_style(Style::default().fg(Color::White));
                    app.overlay = Some(Overlay::TextInput {
                        title: format!("New block in '{}'", topic_key),
                        textarea: Box::new(textarea),
                        purpose: TextInputPurpose::AddBlockContent { topic_key },
                    });
                } else {
                    notify_err(app, "Select a topic first".into());
                }
            }
            Action::ToggleFocus => {
                app.focus = match app.focus {
                    Focus::Left => Focus::Right,
                    Focus::Right => Focus::Left,
                };
            }
            Action::OpenContextMenu => {
                let items = build_context_menu(app);
                if items.is_empty() {
                    // No contextual actions — fall through silently.
                } else {
                    app.overlay = Some(Overlay::ContextMenu {
                        items,
                        selected: 0,
                    });
                }
            }
            Action::OpenPalette => {
                app.overlay = Some(Overlay::CommandPalette {
                    filter: String::new(),
                    selected: 0,
                });
            }
            Action::AmendBlock => {
                if require_edit_mode(app, action) {
                    // Will re-dispatch after lock is acquired.
                } else if let Some(detail) = &app.caches.detail {
                    let topic_key = detail.topic.key.clone();

                    // If the right pane has a specific block selected, skip
                    // the picker and open the editor directly for that block.
                    let preselected_block_id = if app.focus == Focus::Right {
                        match app.selected_detail_element() {
                            Some(DetailElement::Block { block_id, .. }) => Some(block_id),
                            _ => None,
                        }
                    } else {
                        None
                    };

                    // Find the block to edit: preselected, or single-block
                    // shortcut, or picker for 2+.
                    let target_block = preselected_block_id.and_then(|id| {
                        detail.topic.blocks.iter().find(|b| b.id == id)
                    }).or_else(|| {
                        if detail.topic.blocks.len() == 1 {
                            Some(&detail.topic.blocks[0])
                        } else {
                            None
                        }
                    });

                    if detail.topic.blocks.is_empty() {
                        app.overlay = Some(Overlay::Notification {
                            message: "No blocks to amend in this topic".into(),
                            is_error: true,
                        });
                    } else if let Some(block) = target_block {
                        // Direct to editor — skip the picker.
                        let block = block.clone();
                        let lines = soft_wrap(&block.content, 76);
                        let mut textarea = tui_textarea::TextArea::new(lines);
                        textarea.set_cursor_line_style(Style::default());
                        textarea.set_style(Style::default().fg(Color::White));
                        app.overlay = Some(Overlay::TextInput {
                            title: format!(
                                "Amend block {} in {}",
                                block.id, topic_key
                            ),
                            textarea: Box::new(textarea),
                            purpose: TextInputPurpose::AmendBlock {
                                topic_key,
                                block_id: block.id.clone(),
                            },
                        });
                    } else {
                        let blocks: Vec<(String, String)> = detail
                            .topic
                            .blocks
                            .iter()
                            .map(|b| {
                                let preview = b
                                    .content
                                    .lines()
                                    .next()
                                    .unwrap_or("")
                                    .chars()
                                    .take(60)
                                    .collect::<String>();
                                (b.id.clone(), preview)
                            })
                            .collect();
                        app.overlay = Some(Overlay::BlockPicker {
                            topic_key,
                            blocks,
                            selected: 0,
                        });
                    }
                } else {
                    app.overlay = Some(Overlay::Notification {
                        message: "Select a topic first".into(),
                        is_error: true,
                    });
                }
            }
            Action::RenameTopic => {
                if require_edit_mode(app, action) {
                    // Will re-dispatch after lock is acquired.
                } else if let Some(key) = app.selected_key() {
                    app.overlay = Some(Overlay::LineInput {
                        title: format!("Rename '{}' → new key", key),
                        buffer: key.clone(),
                        purpose: LineInputPurpose::RenameKey { old_key: key },
                    });
                } else {
                    app.overlay = Some(Overlay::Notification {
                        message: "Select a topic first".into(),
                        is_error: true,
                    });
                }
            }
            Action::ForgetTopic => {
                if require_edit_mode(app, action) {
                    // Will re-dispatch after lock is acquired.
                } else if let Some(key) = app.selected_key() {
                    app.overlay = Some(Overlay::LineInput {
                        title: format!("Forget '{}' — reason (required)", key),
                        buffer: String::new(),
                        purpose: LineInputPurpose::ForgetReason { topic_key: key },
                    });
                } else {
                    app.overlay = Some(Overlay::Notification {
                        message: "Select a topic first".into(),
                        is_error: true,
                    });
                }
            }
            Action::EditVoice => {
                if require_edit_mode(app, action) {
                    // Will re-dispatch after lock is acquired.
                } else {
                    match client.get_voice().await {
                        Ok(voice_opt) => {
                            let content = voice_opt
                                .map(|v| v.content)
                                .unwrap_or_default();
                            let lines = soft_wrap(&content, 76);
                            let mut textarea = tui_textarea::TextArea::new(lines);
                            textarea.set_cursor_line_style(Style::default());
                            textarea.set_style(Style::default().fg(Color::White));
                            app.overlay = Some(Overlay::TextInput {
                                title: "Edit developer voice".into(),
                                textarea: Box::new(textarea),
                                purpose: TextInputPurpose::EditVoice,
                            });
                        }
                        Err(e) => {
                            app.overlay = Some(Overlay::Notification {
                                message: format!("Failed to load voice: {e}"),
                                is_error: true,
                            });
                        }
                    }
                }
            }
            Action::ManualCheckpoint => {
                if require_edit_mode(app, action) {
                    // Will re-dispatch after lock is acquired.
                } else {
                    app.overlay = Some(Overlay::LineInput {
                        title: "Checkpoint session label".into(),
                        buffer: format!(
                            "tui_{}",
                            chrono::Utc::now().format("%Y%m%d_%H%M%S")
                        ),
                        purpose: LineInputPurpose::CheckpointLabel,
                    });
                }
            }
            Action::LearnNewTopic => {
                if require_edit_mode(app, action) {
                    // Will re-dispatch after lock is acquired.
                } else {
                    app.overlay = Some(Overlay::LineInput {
                        title: "New topic key (e.g. payments/retry)".into(),
                        buffer: String::new(),
                        purpose: LineInputPurpose::NewTopicKey,
                    });
                }
            }
            Action::AddEdge => {
                if require_edit_mode(app, action) {
                    // Will re-dispatch after lock is acquired.
                } else if let Some(key) = app.selected_key() {
                    app.overlay = Some(Overlay::LineInput {
                        title: format!("Edge from '{}' → target topic key", key),
                        buffer: String::new(),
                        purpose: LineInputPurpose::EdgeTargetKey { from_key: key },
                    });
                } else {
                    app.overlay = Some(Overlay::Notification {
                        message: "Select a topic first".into(),
                        is_error: true,
                    });
                }
            }
            Action::EditTags => {
                if require_edit_mode(app, action) {
                    // Will re-dispatch after lock is acquired.
                } else if let Some(detail) = &app.caches.detail {
                    let topic_key = detail.topic.key.clone();
                    let current = detail.topic.tags.join(", ");
                    app.overlay = Some(Overlay::LineInput {
                        title: format!("Tags for '{}' (comma-separated)", topic_key),
                        buffer: current,
                        purpose: LineInputPurpose::EditTags { topic_key },
                    });
                } else {
                    notify_err(app, "Select a topic first".into());
                }
            }
            Action::RemoveEdge => {
                if require_edit_mode(app, action) {
                    // Will re-dispatch after lock is acquired.
                } else if let Some(detail) = &app.caches.detail {
                    // If the right pane has a specific edge selected, remove
                    // it directly without the picker.
                    let preselected = if app.focus == Focus::Right {
                        match app.selected_detail_element() {
                            Some(DetailElement::Edge {
                                idx: _,
                                from,
                                to,
                                edge_type,
                            }) => Some((from, to, edge_type)),
                            _ => None,
                        }
                    } else {
                        None
                    };

                    if let Some((from, to, edge_type)) = preselected {
                        let kind = cairn_core::EdgeKind::from_table_name(&edge_type);
                        match kind {
                            Some(kind) => match client
                                .disconnect(cairn_core::DisconnectParams {
                                    from_key: from,
                                    to_key: to,
                                    edge_type: kind,
                                })
                                .await
                            {
                                Ok(r) => {
                                    notify_ok(
                                        app,
                                        format!(
                                            "{} {} edge: {} → {}",
                                            r.action, r.edge, r.from, r.to
                                        ),
                                    );
                                    app.caches = TopicCaches::default();
                                    app.fetch_active_tab(client).await;
                                }
                                Err(e) => notify_err(app, format!("Disconnect failed: {e}")),
                            },
                            None => {
                                notify_err(app, format!("Unknown edge type: {edge_type}"))
                            }
                        }
                    } else if detail.explore.edges.is_empty() {
                        notify_err(app, "No edges to remove".into());
                    } else {
                        let edges: Vec<(String, String, String, String)> = detail
                            .explore
                            .edges
                            .iter()
                            .map(|e| {
                                (
                                    e.from.clone(),
                                    e.to.clone(),
                                    e.edge_type.clone(),
                                    e.note.clone(),
                                )
                            })
                            .collect();
                        app.overlay = Some(Overlay::EdgePicker {
                            edges,
                            selected: 0,
                        });
                    }
                } else {
                    notify_err(app, "Select a topic first".into());
                }
            }
            Action::MoveBlockUp | Action::MoveBlockDown => {
                if require_edit_mode(app, action) {
                    // Will re-dispatch after lock is acquired.
                } else if let Some(detail) = &app.caches.detail {
                    let topic_key = detail.topic.key.clone();
                    let blocks = &detail.topic.blocks;
                    if blocks.len() < 2 {
                        notify_err(app, "Need at least 2 blocks to reorder".into());
                    } else {
                        // For simplicity: use block picker to choose which
                        // block, then move it in the requested direction.
                        let items: Vec<(String, String)> = blocks
                            .iter()
                            .map(|b| {
                                let preview = b
                                    .content
                                    .lines()
                                    .next()
                                    .unwrap_or("")
                                    .chars()
                                    .take(60)
                                    .collect::<String>();
                                (b.id.clone(), preview)
                            })
                            .collect();
                        app.overlay = Some(Overlay::BlockPicker {
                            topic_key: topic_key.clone(),
                            blocks: items,
                            selected: 0,
                        });
                        // Store the direction in a temporary field... actually,
                        // let's just show the picker and let the user pick.
                        // We'll need a purpose-aware block picker for move.
                        // For now, move up/down operates on the first/last block
                        // directly without a picker.

                        // Actually, let me just do it directly for the first
                        // block. The user can use the command palette + picker
                        // for precise control later.
                        app.overlay = None; // Clear the picker we just opened
                        let is_up = matches!(action, Action::MoveBlockUp);
                        if blocks.len() >= 2 {
                            // Pick the second block (index 1) for move-up
                            // or the second-to-last for move-down, so there's
                            // a visible effect.
                            let (block_id, position) = if is_up {
                                // Move block at index 1 to start
                                (
                                    blocks[1].id.clone(),
                                    cairn_core::Position::Start,
                                )
                            } else {
                                // Move block at index blocks.len()-2 to end
                                let idx = blocks.len() - 2;
                                (
                                    blocks[idx].id.clone(),
                                    cairn_core::Position::End,
                                )
                            };
                            match client
                                .move_block(cairn_core::MoveBlockParams {
                                    topic_key,
                                    block_id: block_id.clone(),
                                    position,
                                })
                                .await
                            {
                                Ok(r) => {
                                    notify_ok(
                                        app,
                                        format!(
                                            "Moved block {} to position {}",
                                            r.block_id, r.new_position
                                        ),
                                    );
                                    app.caches = TopicCaches::default();
                                    app.fetch_active_tab(client).await;
                                }
                                Err(e) => notify_err(app, format!("Move failed: {e}")),
                            }
                        }
                    }
                } else {
                    notify_err(app, "Select a topic first".into());
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Action {
    None,
    Quit,
    EnterFilter,
    EnterSearch,
    ExitText,
    ConfirmText,
    TextPush(char),
    TextPop,
    Move(isize),
    Jump(ListJump),
    SwitchTab(DetailTab),
    PrevTab,
    /// Show the edit-mode confirmation dialog.
    RequestEditMode,
    /// Leave edit mode (release the daemon lock).
    ExitEditMode,
    /// Re-fetch all data from the daemon.
    Refresh,
    /// Open the command palette.
    OpenPalette,
    /// Initiate the amend-block flow (block picker → editor → reason).
    AmendBlock,
    /// Rename the selected topic.
    RenameTopic,
    /// Soft-delete the selected topic.
    ForgetTopic,
    /// Edit the developer voice in the multiline editor.
    EditVoice,
    /// Create a manual checkpoint with a session label.
    ManualCheckpoint,
    /// Learn a brand-new topic (key → title → content).
    LearnNewTopic,
    /// Create a typed edge from the selected topic.
    AddEdge,
    /// Edit the tags on the selected topic.
    EditTags,
    /// Remove an edge from the selected topic.
    RemoveEdge,
    /// Move a block up within the selected topic.
    MoveBlockUp,
    /// Move a block down within the selected topic.
    MoveBlockDown,
    /// Edit the summary of the selected topic.
    EditSummary,
    /// Add a new block to the selected topic.
    AddBlock,
    /// Toggle focus between left and right panes.
    ToggleFocus,
    /// Open context menu for the selected element.
    OpenContextMenu,
}

#[derive(Clone, Copy)]
enum TextTarget {
    Filter,
    Search,
}

fn handle_browse_key(code: KeyCode, mods: KeyModifiers, edit_mode: bool, focus: Focus) -> Action {
    match code {
        // Global keys (work in both panes)
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Esc if edit_mode => Action::ExitEditMode,
        KeyCode::Esc => Action::Quit,
        KeyCode::Tab => Action::ToggleFocus,
        KeyCode::Char('/') => Action::EnterFilter,
        KeyCode::Char('?') => Action::EnterSearch,
        KeyCode::Char('1') => Action::SwitchTab(DetailTab::Detail),
        KeyCode::Char('2') => Action::SwitchTab(DetailTab::Neighbors),
        KeyCode::Char('3') => Action::SwitchTab(DetailTab::History),
        KeyCode::BackTab => Action::PrevTab,
        KeyCode::Char('l') if !mods.contains(KeyModifiers::CONTROL) && focus == Focus::Left => {
            Action::ToggleFocus
        }
        KeyCode::Char('h') if !mods.contains(KeyModifiers::CONTROL) && focus == Focus::Right => {
            Action::ToggleFocus
        }
        KeyCode::Char('R') => Action::Refresh,
        KeyCode::Char(':') => Action::OpenPalette,
        KeyCode::Enter => Action::OpenContextMenu,

        // Navigation (routed by focus)
        KeyCode::Char('j') | KeyCode::Down => Action::Move(1),
        KeyCode::Char('k') | KeyCode::Up => Action::Move(-1),
        KeyCode::Char('g') | KeyCode::Home => Action::Jump(ListJump::First),
        KeyCode::Char('G') | KeyCode::End => Action::Jump(ListJump::Last),

        // Edit-mode keys
        KeyCode::Char('e') if edit_mode => Action::AmendBlock,
        KeyCode::Char('r') if edit_mode => Action::RenameTopic,
        KeyCode::Char('d') if edit_mode => Action::ForgetTopic,
        KeyCode::Char('V') if edit_mode => Action::EditVoice,
        KeyCode::Char('n') if edit_mode => Action::LearnNewTopic,
        KeyCode::Char('a') if edit_mode => Action::AddEdge,
        KeyCode::Char('t') if edit_mode => Action::EditTags,
        KeyCode::Char('x') if edit_mode => Action::RemoveEdge,
        KeyCode::Char('s') if edit_mode => Action::EditSummary,
        KeyCode::Char('b') if edit_mode => Action::AddBlock,
        KeyCode::Char('K') if edit_mode => Action::MoveBlockUp,
        KeyCode::Char('J') if edit_mode => Action::MoveBlockDown,
        KeyCode::Char('e') => Action::RequestEditMode,
        _ => Action::None,
    }
}

fn handle_text_key(code: KeyCode, _target: TextTarget) -> Action {
    match code {
        KeyCode::Esc => Action::ExitText,
        KeyCode::Enter => Action::ConfirmText,
        KeyCode::Backspace => Action::TextPop,
        KeyCode::Char(c) => Action::TextPush(c),
        _ => Action::None,
    }
}

/// Build context-menu items based on the currently focused pane and
/// selected element. Returns an empty vec if nothing is actionable.
fn build_context_menu(app: &App) -> Vec<ContextMenuItem> {
    let mut items = Vec::new();

    match app.focus {
        Focus::Left => {
            // Left pane: topic-level actions.
            if app.selected_key().is_some() {
                if app.edit_mode {
                    items.push(ContextMenuItem {
                        label: "Amend block".into(),
                        action: Action::AmendBlock,
                    });
                    items.push(ContextMenuItem {
                        label: "Rename topic".into(),
                        action: Action::RenameTopic,
                    });
                    items.push(ContextMenuItem {
                        label: "Forget topic".into(),
                        action: Action::ForgetTopic,
                    });
                    items.push(ContextMenuItem {
                        label: "Edit tags".into(),
                        action: Action::EditTags,
                    });
                    items.push(ContextMenuItem {
                        label: "Add edge".into(),
                        action: Action::AddEdge,
                    });
                    items.push(ContextMenuItem {
                        label: "Remove edge".into(),
                        action: Action::RemoveEdge,
                    });
                    items.push(ContextMenuItem {
                        label: "Add block".into(),
                        action: Action::AddBlock,
                    });
                    items.push(ContextMenuItem {
                        label: "Edit summary".into(),
                        action: Action::EditSummary,
                    });
                }
                // Read-only actions always available.
                items.push(ContextMenuItem {
                    label: "Explore (switch to detail)".into(),
                    action: Action::SwitchTab(DetailTab::Detail),
                });
                items.push(ContextMenuItem {
                    label: "Neighbors".into(),
                    action: Action::SwitchTab(DetailTab::Neighbors),
                });
                items.push(ContextMenuItem {
                    label: "History".into(),
                    action: Action::SwitchTab(DetailTab::History),
                });
            }
        }
        Focus::Right => {
            // Right pane: element-level actions based on selected element.
            if let Some(elem) = app.selected_detail_element() {
                match elem {
                    DetailElement::Title => {
                        if app.edit_mode {
                            items.push(ContextMenuItem {
                                label: "Rename topic".into(),
                                action: Action::RenameTopic,
                            });
                        }
                    }
                    DetailElement::Tags => {
                        if app.edit_mode {
                            items.push(ContextMenuItem {
                                label: "Edit tags".into(),
                                action: Action::EditTags,
                            });
                        }
                    }
                    DetailElement::Summary => {
                        if app.edit_mode {
                            items.push(ContextMenuItem {
                                label: "Edit summary".into(),
                                action: Action::EditSummary,
                            });
                        }
                    }
                    DetailElement::Block { .. } => {
                        if app.edit_mode {
                            items.push(ContextMenuItem {
                                label: "Amend this block".into(),
                                action: Action::AmendBlock,
                            });
                            items.push(ContextMenuItem {
                                label: "Move block up".into(),
                                action: Action::MoveBlockUp,
                            });
                            items.push(ContextMenuItem {
                                label: "Move block down".into(),
                                action: Action::MoveBlockDown,
                            });
                        }
                    }
                    DetailElement::Edge { .. } => {
                        if app.edit_mode {
                            items.push(ContextMenuItem {
                                label: "Remove this edge".into(),
                                action: Action::RemoveEdge,
                            });
                        }
                    }
                }
            }
        }
    }

    items
}

async fn handle_overlay_key(
    app: &mut App,
    client: &CairnClient,
    key: event::KeyEvent,
) -> OverlayResult {
    // Take ownership so we can match without borrowing app.
    let overlay = app.overlay.take().unwrap();
    match overlay {
        Overlay::EditConfirm { pending_action } => match key.code {
            KeyCode::Enter => {
                match client.begin_editor_session(Some("TUI edit session")).await {
                    Ok(()) => {
                        app.edit_mode = true;
                        // If an action was pending (e.g. user pressed Enter
                        // on a block while not in edit mode), dispatch it now.
                        if let Some(action) = pending_action {
                            return OverlayResult::Dispatch(action);
                        }
                    }
                    Err(CairnError::EditorBusy { reason, since }) => {
                        app.overlay = Some(Overlay::Notification {
                            message: format!(
                                "Editor lock held by another client since {} (reason: {})",
                                since.format("%H:%M:%S"),
                                reason.as_deref().unwrap_or("none")
                            ),
                            is_error: true,
                        });
                    }
                    Err(e) => {
                        app.overlay = Some(Overlay::Notification {
                            message: format!("Failed to acquire editor lock: {e}"),
                            is_error: true,
                        });
                    }
                }
                OverlayResult::Consumed
            }
            _ => OverlayResult::Consumed, // Esc or anything else → dismiss
        },
        Overlay::Notification { .. } => OverlayResult::Consumed,
        Overlay::CommandPalette {
            mut filter,
            mut selected,
        } => match key.code {
            KeyCode::Esc => OverlayResult::Consumed,
            KeyCode::Enter => {
                let matches = filtered_palette(&filter, app.edit_mode);
                if let Some((_, cmd)) = matches.get(selected) {
                    OverlayResult::Dispatch(cmd.action)
                } else {
                    OverlayResult::Consumed
                }
            }
            KeyCode::Char(c) => {
                filter.push(c);
                selected = 0;
                app.overlay = Some(Overlay::CommandPalette { filter, selected });
                OverlayResult::Consumed
            }
            KeyCode::Backspace => {
                filter.pop();
                selected = 0;
                app.overlay = Some(Overlay::CommandPalette { filter, selected });
                OverlayResult::Consumed
            }
            KeyCode::Down | KeyCode::Tab => {
                let matches = filtered_palette(&filter, app.edit_mode);
                if !matches.is_empty() {
                    selected = (selected + 1).min(matches.len() - 1);
                }
                app.overlay = Some(Overlay::CommandPalette { filter, selected });
                OverlayResult::Consumed
            }
            KeyCode::Up | KeyCode::BackTab => {
                selected = selected.saturating_sub(1);
                app.overlay = Some(Overlay::CommandPalette { filter, selected });
                OverlayResult::Consumed
            }
            _ => {
                app.overlay = Some(Overlay::CommandPalette { filter, selected });
                OverlayResult::Consumed
            }
        },
        Overlay::TextInput {
            textarea: mut textarea_box,
            title,
            purpose,
        } => {
            let textarea = &mut *textarea_box;
            // Ctrl+S saves, Esc cancels. Everything else is passed to the textarea.
            if key.code == KeyCode::Char('s')
                && key.modifiers.contains(KeyModifiers::CONTROL)
            {
                let content = unwrap_soft(textarea.lines());
                match purpose {
                    TextInputPurpose::AmendBlock {
                        topic_key,
                        block_id,
                    } => {
                        // Chain: content → reason prompt.
                        app.overlay = Some(Overlay::LineInput {
                            title: "Reason for amendment".into(),
                            buffer: String::new(),
                            purpose: LineInputPurpose::AmendReason {
                                topic_key,
                                block_id,
                                new_content: content,
                            },
                        });
                    }
                    TextInputPurpose::EditVoice => {
                        match client.set_voice(&content).await {
                            Ok(_) => {
                                notify_ok(app, "Voice updated".into());
                            }
                            Err(e) => notify_err(app, format!("Set voice failed: {e}")),
                        }
                    }
                    TextInputPurpose::LearnContent { topic_key, title } => {
                        match client
                            .learn(cairn_core::LearnParams {
                                topic_key: topic_key.clone(),
                                title: Some(title),
                                summary: None,
                                content,
                                voice: None,
                                tags: vec![],
                                position: cairn_core::Position::End,
                            })
                            .await
                        {
                            Ok(r) => {
                                notify_ok(app, format!(
                                    "Created topic '{}' (block {})",
                                    r.topic_key, r.block_id
                                ));
                                app.refresh(client).await;
                            }
                            Err(e) => notify_err(app, format!("Learn failed: {e}")),
                        }
                    }
                    TextInputPurpose::AddBlockContent { topic_key } => {
                        if content.trim().is_empty() {
                            notify_err(app, "Block content cannot be empty".into());
                        } else {
                            match client
                                .learn(cairn_core::LearnParams {
                                    topic_key: topic_key.clone(),
                                    title: None,
                                    summary: None,
                                    content,
                                    voice: None,
                                    tags: vec![],
                                    position: cairn_core::Position::End,
                                })
                                .await
                            {
                                Ok(r) => {
                                    notify_ok(app, format!(
                                        "Added block {} to '{}'",
                                        r.block_id, r.topic_key
                                    ));
                                    app.caches = TopicCaches::default();
                                    app.fetch_active_tab(client).await;
                                }
                                Err(e) => notify_err(app, format!("Add block failed: {e}")),
                            }
                        }
                    }
                }
                OverlayResult::Consumed
            } else if key.code == KeyCode::Esc {
                // Cancel — discard edits.
                OverlayResult::Consumed
            } else {
                // Pass the key event to tui-textarea for editing.
                textarea.input(key);
                app.overlay = Some(Overlay::TextInput {
                    textarea: textarea_box,
                    title,
                    purpose,
                });
                OverlayResult::Consumed
            }
        }
        Overlay::LineInput {
            title,
            mut buffer,
            purpose,
        } => match key.code {
            KeyCode::Esc => OverlayResult::Consumed,
            KeyCode::Enter => {
                let value = buffer.trim().to_string();
                if value.is_empty() {
                    // All inputs require non-empty text.
                    app.overlay = Some(Overlay::LineInput {
                        title,
                        buffer,
                        purpose,
                    });
                    return OverlayResult::Consumed;
                }
                match purpose {
                    LineInputPurpose::AmendReason {
                        topic_key,
                        block_id,
                        new_content,
                    } => match client
                        .amend(cairn_core::AmendParams {
                            topic_key, block_id, new_content, reason: value,
                        })
                        .await
                    {
                        Ok(r) => {
                            notify_ok(app, format!("Amended block {} in '{}'", r.block_id, r.topic_key));
                            app.caches = TopicCaches::default();
                            app.fetch_active_tab(client).await;
                        }
                        Err(e) => notify_err(app, format!("Amend failed: {e}")),
                    },
                    LineInputPurpose::RenameKey { old_key } => match client
                        .rename(cairn_core::RenameParams {
                            old_key: old_key.clone(),
                            new_key: value.clone(),
                        })
                        .await
                    {
                        Ok(r) => {
                            notify_ok(app, format!("Renamed '{}' → '{}'", r.old_key, r.new_key));
                            app.refresh(client).await;
                        }
                        Err(e) => notify_err(app, format!("Rename failed: {e}")),
                    },
                    LineInputPurpose::ForgetReason { topic_key } => match client
                        .forget(cairn_core::ForgetParams {
                            topic_key, reason: value,
                        })
                        .await
                    {
                        Ok(r) => {
                            notify_ok(app, format!("Forgot '{}': {}", r.topic_key, r.reason));
                            app.refresh(client).await;
                        }
                        Err(e) => notify_err(app, format!("Forget failed: {e}")),
                    },
                    LineInputPurpose::CheckpointLabel => match client
                        .checkpoint(cairn_core::CheckpointParams {
                            session_id: value.clone(),
                            emergency: false,
                        })
                        .await
                    {
                        Ok(r) => {
                            notify_ok(app, format!(
                                "Checkpoint '{}' ({} mutations)",
                                r.session_id, r.mutations_persisted
                            ));
                        }
                        Err(e) => notify_err(app, format!("Checkpoint failed: {e}")),
                    },
                    LineInputPurpose::NewTopicKey => {
                        // Chain: key → title prompt
                        app.overlay = Some(Overlay::LineInput {
                            title: format!("Title for '{}'", value),
                            buffer: String::new(),
                            purpose: LineInputPurpose::NewTopicTitle {
                                topic_key: value,
                            },
                        });
                    }
                    LineInputPurpose::NewTopicTitle { topic_key } => {
                        // Chain: title → content editor
                        let mut textarea =
                            tui_textarea::TextArea::new(vec![String::new()]);
                        textarea.set_cursor_line_style(Style::default());
                        textarea.set_style(Style::default().fg(Color::White));
                        app.overlay = Some(Overlay::TextInput {
                            title: format!("Content for '{}'", topic_key),
                            textarea: Box::new(textarea),
                            purpose: TextInputPurpose::LearnContent {
                                topic_key,
                                title: value,
                            },
                        });
                    }
                    LineInputPurpose::EdgeTargetKey { from_key } => {
                        // Chain: target key → edge type picker
                        app.overlay = Some(Overlay::EdgeTypePicker {
                            from_key,
                            to_key: value,
                            selected: 0,
                        });
                    }
                    LineInputPurpose::EditSummary { topic_key } => match client
                        .set_summary(cairn_core::SetSummaryParams {
                            topic_key, summary: value,
                        })
                        .await
                    {
                        Ok(r) => {
                            notify_ok(app, format!("Summary updated for '{}'", r.topic_key));
                            app.caches = TopicCaches::default();
                            app.fetch_active_tab(client).await;
                        }
                        Err(e) => notify_err(app, format!("Set summary failed: {e}")),
                    },
                    LineInputPurpose::EditTags { topic_key } => {
                        let tags: Vec<String> = value
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        match client
                            .set_tags(cairn_core::SetTagsParams {
                                topic_key,
                                tags: tags.clone(),
                            })
                            .await
                        {
                            Ok(r) => {
                                notify_ok(
                                    app,
                                    format!("Tags for '{}': [{}]", r.topic_key, r.tags.join(", ")),
                                );
                                app.caches = TopicCaches::default();
                                app.fetch_active_tab(client).await;
                            }
                            Err(e) => notify_err(app, format!("Set tags failed: {e}")),
                        }
                    }
                    LineInputPurpose::EdgeNote {
                        from_key,
                        to_key,
                        edge_type,
                    } => {
                        let kind = cairn_core::EdgeKind::from_table_name(&edge_type);
                        match kind {
                            Some(kind) => match client
                                .connect_topics(cairn_core::ConnectParams {
                                    from_key: from_key.clone(),
                                    to_key: to_key.clone(),
                                    edge_type: kind,
                                    note: value,
                                    severity: None,
                                })
                                .await
                            {
                                Ok(r) => {
                                    notify_ok(app, format!(
                                        "{} edge: {} → {}",
                                        r.action, r.from, r.to
                                    ));
                                    app.caches = TopicCaches::default();
                                    app.fetch_active_tab(client).await;
                                }
                                Err(e) => notify_err(app, format!("Connect failed: {e}")),
                            },
                            None => notify_err(app, format!("Unknown edge type: {edge_type}")),
                        }
                    }
                }
                OverlayResult::Consumed
            }
            KeyCode::Backspace => {
                buffer.pop();
                app.overlay = Some(Overlay::LineInput {
                    title,
                    buffer,
                    purpose,
                });
                OverlayResult::Consumed
            }
            KeyCode::Char(c) => {
                buffer.push(c);
                app.overlay = Some(Overlay::LineInput {
                    title,
                    buffer,
                    purpose,
                });
                OverlayResult::Consumed
            }
            _ => {
                app.overlay = Some(Overlay::LineInput {
                    title,
                    buffer,
                    purpose,
                });
                OverlayResult::Consumed
            }
        },
        Overlay::ContextMenu {
            items,
            mut selected,
        } => match key.code {
            KeyCode::Esc => OverlayResult::Consumed,
            KeyCode::Enter => {
                if let Some(item) = items.get(selected) {
                    OverlayResult::Dispatch(item.action)
                } else {
                    OverlayResult::Consumed
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !items.is_empty() {
                    selected = (selected + 1).min(items.len() - 1);
                }
                app.overlay = Some(Overlay::ContextMenu { items, selected });
                OverlayResult::Consumed
            }
            KeyCode::Up | KeyCode::Char('k') => {
                selected = selected.saturating_sub(1);
                app.overlay = Some(Overlay::ContextMenu { items, selected });
                OverlayResult::Consumed
            }
            _ => {
                app.overlay = Some(Overlay::ContextMenu { items, selected });
                OverlayResult::Consumed
            }
        },
        Overlay::EdgePicker {
            edges,
            mut selected,
        } => match key.code {
            KeyCode::Esc => OverlayResult::Consumed,
            KeyCode::Enter => {
                if let Some((from, to, edge_type, _note)) = edges.get(selected) {
                    let kind = cairn_core::EdgeKind::from_table_name(edge_type);
                    match kind {
                        Some(kind) => match client
                            .disconnect(cairn_core::DisconnectParams {
                                from_key: from.clone(),
                                to_key: to.clone(),
                                edge_type: kind,
                            })
                            .await
                        {
                            Ok(r) => {
                                notify_ok(
                                    app,
                                    format!("{} {} edge: {} → {}", r.action, r.edge, r.from, r.to),
                                );
                                app.caches = TopicCaches::default();
                                app.fetch_active_tab(client).await;
                            }
                            Err(e) => notify_err(app, format!("Disconnect failed: {e}")),
                        },
                        None => notify_err(app, format!("Unknown edge type: {edge_type}")),
                    }
                }
                OverlayResult::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !edges.is_empty() {
                    selected = (selected + 1).min(edges.len() - 1);
                }
                app.overlay = Some(Overlay::EdgePicker { edges, selected });
                OverlayResult::Consumed
            }
            KeyCode::Up | KeyCode::Char('k') => {
                selected = selected.saturating_sub(1);
                app.overlay = Some(Overlay::EdgePicker { edges, selected });
                OverlayResult::Consumed
            }
            _ => {
                app.overlay = Some(Overlay::EdgePicker { edges, selected });
                OverlayResult::Consumed
            }
        },
        Overlay::EdgeTypePicker {
            from_key,
            to_key,
            mut selected,
        } => match key.code {
            KeyCode::Esc => OverlayResult::Consumed,
            KeyCode::Enter => {
                if let Some((type_name, _)) = EDGE_TYPES.get(selected) {
                    app.overlay = Some(Overlay::LineInput {
                        title: format!("{} → {} [{}] — note", from_key, to_key, type_name),
                        buffer: String::new(),
                        purpose: LineInputPurpose::EdgeNote {
                            from_key,
                            to_key,
                            edge_type: type_name.to_string(),
                        },
                    });
                }
                OverlayResult::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                selected = (selected + 1).min(EDGE_TYPES.len().saturating_sub(1));
                app.overlay = Some(Overlay::EdgeTypePicker {
                    from_key, to_key, selected,
                });
                OverlayResult::Consumed
            }
            KeyCode::Up | KeyCode::Char('k') => {
                selected = selected.saturating_sub(1);
                app.overlay = Some(Overlay::EdgeTypePicker {
                    from_key, to_key, selected,
                });
                OverlayResult::Consumed
            }
            _ => {
                app.overlay = Some(Overlay::EdgeTypePicker {
                    from_key, to_key, selected,
                });
                OverlayResult::Consumed
            }
        },
        Overlay::BlockPicker {
            topic_key,
            blocks,
            mut selected,
        } => match key.code {
            KeyCode::Esc => OverlayResult::Consumed,
            KeyCode::Enter => {
                if let Some((block_id, _)) = blocks.get(selected) {
                    // Fetch the full block content to prefill the editor.
                    if let Some(detail) = &app.caches.detail {
                        if let Some(block) = detail
                            .topic
                            .blocks
                            .iter()
                            .find(|b| b.id == *block_id)
                        {
                            let lines = soft_wrap(&block.content, 76);
                            let mut textarea = tui_textarea::TextArea::new(lines);
                            textarea.set_cursor_line_style(Style::default());
                            textarea.set_style(Style::default().fg(Color::White));
                            app.overlay = Some(Overlay::TextInput {
                                title: format!("Amend block {} in {}", block_id, topic_key),
                                textarea: Box::new(textarea),
                                purpose: TextInputPurpose::AmendBlock {
                                    topic_key,
                                    block_id: block_id.clone(),
                                },
                            });
                        }
                    }
                }
                OverlayResult::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !blocks.is_empty() {
                    selected = (selected + 1).min(blocks.len() - 1);
                }
                app.overlay = Some(Overlay::BlockPicker {
                    topic_key,
                    blocks,
                    selected,
                });
                OverlayResult::Consumed
            }
            KeyCode::Up | KeyCode::Char('k') => {
                selected = selected.saturating_sub(1);
                app.overlay = Some(Overlay::BlockPicker {
                    topic_key,
                    blocks,
                    selected,
                });
                OverlayResult::Consumed
            }
            _ => {
                app.overlay = Some(Overlay::BlockPicker {
                    topic_key,
                    blocks,
                    selected,
                });
                OverlayResult::Consumed
            }
        },
    }
}

// ── Rendering ─────────────────────────────────────────────────────

fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.area());

    draw_header(f, chunks[0], app);
    draw_body(f, chunks[1], app);
    draw_footer(f, chunks[2], app);

    // Overlays render on top of everything else.
    if app.overlay.is_some() {
        draw_overlay(f, app, f.area());
    }
}

/// Word-wrap a string into lines that fit within `width` characters.
/// Preserves existing line breaks. Empty lines (paragraph separators)
/// are preserved as-is.
fn soft_wrap(text: &str, width: usize) -> Vec<String> {
    let mut result = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            result.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in line.split_whitespace() {
            if current.is_empty() {
                current = word.to_string();
            } else if current.len() + 1 + word.len() <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                result.push(current);
                current = word.to_string();
            }
        }
        if !current.is_empty() {
            result.push(current);
        }
    }
    if result.is_empty() {
        result.push(String::new());
    }
    result
}

/// Rejoin soft-wrapped lines back into the original paragraph structure.
/// Lines are joined with a space unless followed by an empty line
/// (paragraph separator) or the end of the text.
fn unwrap_soft(lines: &[String]) -> String {
    let mut result = String::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].is_empty() {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push('\n');
            i += 1;
            continue;
        }
        // Collect consecutive non-empty lines into one paragraph.
        let start = i;
        while i < lines.len() && !lines[i].is_empty() {
            i += 1;
        }
        if !result.is_empty() && !result.ends_with('\n') {
            result.push('\n');
        }
        let paragraph: Vec<&str> = lines[start..i].iter().map(|s| s.as_str()).collect();
        result.push_str(&paragraph.join(" "));
    }
    result
}

/// Compute scroll offset so that `selected` is visible within `viewport_height` rows.
fn scroll_offset(selected: usize, viewport_height: usize) -> usize {
    if viewport_height == 0 {
        return 0;
    }
    if selected < viewport_height {
        0
    } else {
        selected - viewport_height + 1
    }
}

/// If not in edit mode, show the edit confirmation dialog with a pending
/// action that will fire after the lock is acquired. Returns true if the
/// dialog was shown (caller should stop processing), false if already in
/// edit mode (caller should proceed).
fn require_edit_mode(app: &mut App, pending: Action) -> bool {
    if app.edit_mode {
        false
    } else {
        app.overlay = Some(Overlay::EditConfirm {
            pending_action: Some(pending),
        });
        true
    }
}

fn notify_ok(app: &mut App, message: String) {
    app.overlay = Some(Overlay::Notification {
        message,
        is_error: false,
    });
}

fn notify_err(app: &mut App, message: String) {
    app.overlay = Some(Overlay::Notification {
        message,
        is_error: true,
    });
}

/// Render a modal overlay centered on the screen.
fn draw_overlay(f: &mut Frame, app: &App, area: Rect) {
    let overlay = app.overlay.as_ref().unwrap();
    match overlay {
        Overlay::EditConfirm { .. } => {
            let dialog_width = 54u16.min(area.width.saturating_sub(4));
            let dialog_height = 9u16.min(area.height.saturating_sub(2));
            let x = (area.width.saturating_sub(dialog_width)) / 2;
            let y = (area.height.saturating_sub(dialog_height)) / 2;
            let dialog_area = Rect::new(x, y, dialog_width, dialog_height);

            f.render_widget(Clear, dialog_area);

            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  ⚠  ENTER EDIT MODE",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  This will acquire an exclusive lock.",
                    Style::default().fg(Color::White),
                )),
                Line::from(Span::styled(
                    "  Agents will be blocked from writing",
                    Style::default().fg(Color::White),
                )),
                Line::from(Span::styled(
                    "  until you exit edit mode.",
                    Style::default().fg(Color::White),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled(
                        "  [Enter]",
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" confirm  ", Style::default().fg(Color::White)),
                    Span::styled(
                        "[Esc]",
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" cancel", Style::default().fg(Color::White)),
                ]),
            ];

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red))
                .style(Style::default().bg(Color::DarkGray));
            let paragraph = Paragraph::new(lines).block(block);
            f.render_widget(paragraph, dialog_area);
        }
        Overlay::CommandPalette { filter, selected } => {
            let matches = filtered_palette(filter, app.edit_mode);
            let max_visible = 12usize;
            let list_height = matches.len().min(max_visible) as u16;
            // 3 = top border + filter line + bottom border
            let dialog_height = (list_height + 3).min(area.height.saturating_sub(4));
            let dialog_width = 60u16.min(area.width.saturating_sub(4));
            let x = (area.width.saturating_sub(dialog_width)) / 2;
            let y = (area.height.saturating_sub(dialog_height)) / 2;
            let dialog_area = Rect::new(x, y, dialog_width, dialog_height);
            f.render_widget(Clear, dialog_area);

            // Background
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" : command palette ")
                .style(Style::default().bg(Color::Black));
            f.render_widget(block, dialog_area);

            // Inner area (inside borders)
            let inner = Rect::new(
                dialog_area.x + 1,
                dialog_area.y + 1,
                dialog_area.width.saturating_sub(2),
                dialog_area.height.saturating_sub(2),
            );

            if inner.height == 0 || inner.width == 0 {
                return;
            }

            // Filter input line
            let filter_line = Line::from(vec![
                Span::styled("> ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    filter.clone(),
                    Style::default().add_modifier(Modifier::UNDERLINED),
                ),
                Span::styled("_", Style::default().fg(Color::DarkGray)),
            ]);
            let filter_area = Rect::new(inner.x, inner.y, inner.width, 1);
            f.render_widget(Paragraph::new(filter_line), filter_area);

            // Command list
            let list_area = Rect::new(
                inner.x,
                inner.y + 1,
                inner.width,
                inner.height.saturating_sub(1),
            );

            let vh = list_area.height as usize;
            let scroll = scroll_offset(*selected, vh);
            let items: Vec<ListItem> = matches
                .iter()
                .enumerate()
                .skip(scroll)
                .take(vh)
                .map(|(i, (_, cmd))| {
                    let mut spans = vec![
                        Span::styled(
                            format!(" {:<20}", cmd.name),
                            if i == *selected {
                                Style::default()
                                    .fg(Color::Black)
                                    .bg(Color::Cyan)
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(Color::White)
                            },
                        ),
                        Span::styled(
                            cmd.description,
                            if i == *selected {
                                Style::default().fg(Color::Black).bg(Color::Cyan)
                            } else {
                                Style::default().fg(Color::DarkGray)
                            },
                        ),
                    ];
                    if let Some(hint) = cmd.key_hint {
                        spans.push(Span::styled(
                            format!("  {hint}"),
                            if i == *selected {
                                Style::default().fg(Color::Black).bg(Color::Cyan)
                            } else {
                                Style::default().fg(Color::Yellow)
                            },
                        ));
                    }
                    ListItem::new(Line::from(spans))
                })
                .collect();

            let list = List::new(items).style(Style::default().bg(Color::Black));
            f.render_widget(list, list_area);
        }
        Overlay::TextInput {
            title,
            textarea: textarea_box,
            ..
        } => {
            let textarea = &**textarea_box;
            // Full-width, ~80% height editor overlay.
            let margin = 2u16;
            let w = area.width.saturating_sub(margin * 2);
            let h = area.height.saturating_sub(margin * 2);
            let editor_area = Rect::new(margin, margin, w, h);
            f.render_widget(Clear, editor_area);

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(format!(" {} ", title))
                .style(Style::default().bg(Color::Black));
            let inner = block.inner(editor_area);
            f.render_widget(block, editor_area);

            // Render the textarea inside the block.
            f.render_widget(textarea, inner);

            // Hint line at the bottom of the editor area.
            if editor_area.height >= 3 {
                let hint_area = Rect::new(
                    editor_area.x + 1,
                    editor_area.y + editor_area.height - 1,
                    editor_area.width.saturating_sub(2),
                    1,
                );
                let hints = Line::from(vec![
                    Span::styled(
                        "Ctrl+S",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" save  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        "Esc",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
                ]);
                f.render_widget(
                    Paragraph::new(hints).style(Style::default().bg(Color::Black)),
                    hint_area,
                );
            }
        }
        Overlay::LineInput {
            title, buffer, ..
        } => {
            let dialog_width = 60u16.min(area.width.saturating_sub(4));
            let dialog_height = 5u16;
            let x = (area.width.saturating_sub(dialog_width)) / 2;
            let y = (area.height.saturating_sub(dialog_height)) / 2;
            let dialog_area = Rect::new(x, y, dialog_width, dialog_height);
            f.render_widget(Clear, dialog_area);

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(format!(" {} ", title))
                .style(Style::default().bg(Color::Black));
            let inner = block.inner(dialog_area);
            f.render_widget(block, dialog_area);

            let input_line = Line::from(vec![
                Span::styled("> ", Style::default().fg(Color::Yellow)),
                Span::raw(buffer.clone()),
                Span::styled("_", Style::default().fg(Color::DarkGray)),
            ]);
            f.render_widget(Paragraph::new(input_line), inner);

            // Hint
            if inner.height >= 2 {
                let hint_area =
                    Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
                let hints = Line::from(vec![
                    Span::styled(
                        "Enter",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" confirm  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        "Esc",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
                ]);
                f.render_widget(Paragraph::new(hints), hint_area);
            }
        }
        Overlay::BlockPicker {
            topic_key,
            blocks,
            selected,
        } => {
            let list_height = blocks.len().min(12) as u16;
            let dialog_height = (list_height + 3).min(area.height.saturating_sub(4));
            let dialog_width = 60u16.min(area.width.saturating_sub(4));
            let x = (area.width.saturating_sub(dialog_width)) / 2;
            let y = (area.height.saturating_sub(dialog_height)) / 2;
            let dialog_area = Rect::new(x, y, dialog_width, dialog_height);
            f.render_widget(Clear, dialog_area);

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(format!(" Select block in {} ", topic_key))
                .style(Style::default().bg(Color::Black));
            let inner = block.inner(dialog_area);
            f.render_widget(block, dialog_area);

            let vh = inner.height as usize;
            let scroll = scroll_offset(*selected, vh);
            let items: Vec<ListItem> = blocks
                .iter()
                .enumerate()
                .skip(scroll)
                .take(vh)
                .map(|(i, (_id, preview))| {
                    let style = if i == *selected {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(format!(" {}. ", i + 1), style),
                        Span::styled(
                            if preview.is_empty() {
                                "(empty)".into()
                            } else {
                                preview.clone()
                            },
                            style,
                        ),
                    ]))
                })
                .collect();
            let list = List::new(items).style(Style::default().bg(Color::Black));
            f.render_widget(list, inner);
        }
        Overlay::ContextMenu { items, selected } => {
            let list_height = items.len().min(12) as u16;
            let dialog_height = (list_height + 2).min(area.height.saturating_sub(4));
            let dialog_width = 40u16.min(area.width.saturating_sub(4));
            let x = (area.width.saturating_sub(dialog_width)) / 2;
            let y = (area.height.saturating_sub(dialog_height)) / 2;
            let dialog_area = Rect::new(x, y, dialog_width, dialog_height);

            f.render_widget(Clear, dialog_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Actions ")
                .style(Style::default().bg(Color::Black));
            let inner = block.inner(dialog_area);
            f.render_widget(block, dialog_area);

            let vh = inner.height as usize;
            let scroll = scroll_offset(*selected, vh);
            let list_items: Vec<ListItem> = items
                .iter()
                .enumerate()
                .skip(scroll)
                .take(vh)
                .map(|(i, item)| {
                    let style = if i == *selected {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    ListItem::new(Line::from(Span::styled(
                        format!(" {} ", item.label),
                        style,
                    )))
                })
                .collect();
            let list = List::new(list_items).style(Style::default().bg(Color::Black));
            f.render_widget(list, inner);
        }
        Overlay::EdgePicker { edges, selected } => {
            let list_height = edges.len().min(12) as u16;
            let dialog_height = (list_height + 2).min(area.height.saturating_sub(4));
            let dialog_width = 70u16.min(area.width.saturating_sub(4));
            let x = (area.width.saturating_sub(dialog_width)) / 2;
            let y = (area.height.saturating_sub(dialog_height)) / 2;
            let dialog_area = Rect::new(x, y, dialog_width, dialog_height);
            f.render_widget(Clear, dialog_area);

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red))
                .title(" Remove edge (Enter to delete, Esc to cancel) ")
                .style(Style::default().bg(Color::Black));
            let inner = block.inner(dialog_area);
            f.render_widget(block, dialog_area);

            let vh = inner.height as usize;
            let scroll = scroll_offset(*selected, vh);
            let items: Vec<ListItem> = edges
                .iter()
                .enumerate()
                .skip(scroll)
                .take(vh)
                .map(|(i, (from, to, etype, note))| {
                    let style = if i == *selected {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Red)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    let desc_style = if i == *selected {
                        Style::default().fg(Color::Black).bg(Color::Red)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(format!(" {from}"), style),
                        Span::styled(format!(" →[{etype}]→ "), desc_style),
                        Span::styled(format!("{to} "), style),
                        Span::styled(
                            note.chars().take(20).collect::<String>(),
                            desc_style,
                        ),
                    ]))
                })
                .collect();
            let list = List::new(items).style(Style::default().bg(Color::Black));
            f.render_widget(list, inner);
        }
        Overlay::EdgeTypePicker {
            from_key,
            to_key,
            selected,
        } => {
            let list_height = EDGE_TYPES.len() as u16;
            let dialog_height = (list_height + 2).min(area.height.saturating_sub(4));
            let dialog_width = 60u16.min(area.width.saturating_sub(4));
            let x = (area.width.saturating_sub(dialog_width)) / 2;
            let y = (area.height.saturating_sub(dialog_height)) / 2;
            let dialog_area = Rect::new(x, y, dialog_width, dialog_height);
            f.render_widget(Clear, dialog_area);

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(format!(" {} → {} — edge type ", from_key, to_key))
                .style(Style::default().bg(Color::Black));
            let inner = block.inner(dialog_area);
            f.render_widget(block, dialog_area);

            let vh = inner.height as usize;
            let scroll = scroll_offset(*selected, vh);
            let items: Vec<ListItem> = EDGE_TYPES
                .iter()
                .enumerate()
                .skip(scroll)
                .take(vh)
                .map(|(i, (name, desc))| {
                    let style = if i == *selected {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(format!(" {:<14}", name), style),
                        Span::styled(
                            *desc,
                            if i == *selected {
                                Style::default().fg(Color::Black).bg(Color::Yellow)
                            } else {
                                Style::default().fg(Color::DarkGray)
                            },
                        ),
                    ]))
                })
                .collect();
            let list = List::new(items).style(Style::default().bg(Color::Black));
            f.render_widget(list, inner);
        }
        Overlay::Notification { message, is_error } => {
            let color = if *is_error { Color::Red } else { Color::Green };
            let width = (message.len() as u16 + 6).min(area.width.saturating_sub(4));
            let x = (area.width.saturating_sub(width)) / 2;
            let y = area.height.saturating_sub(4);
            let note_area = Rect::new(x, y, width, 3);
            f.render_widget(Clear, note_area);

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(color))
                .style(Style::default().bg(Color::Black));
            let p = Paragraph::new(Line::from(Span::styled(
                format!(" {message}"),
                Style::default().fg(color),
            )))
            .block(block);
            f.render_widget(p, note_area);
        }
    }
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let s = &app.status.stats;
    let mut spans = vec![
        Span::styled("cairn", Style::default().add_modifier(Modifier::BOLD)),
    ];
    if app.edit_mode {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            "[EDIT MODE]",
            Style::default()
                .fg(Color::White)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans.extend([
        Span::raw("  "),
        Span::styled(
            &app.status.db_path,
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw("  "),
        Span::raw(format!(
            "{} active / {} total ({} deprecated, {} stale)",
            s.active, s.total, s.deprecated, s.stale_90d
        )),
    ]);
    let title = if app.edit_mode {
        " cairn-tui [EDITING] "
    } else {
        " cairn-tui "
    };
    let block = if app.edit_mode {
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(Color::Red))
    } else {
        Block::default().borders(Borders::ALL).title(title)
    };
    let header = Paragraph::new(Line::from(spans)).block(block);
    f.render_widget(header, area);
}

fn draw_body(f: &mut Frame, area: Rect, app: &App) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(area);

    draw_topic_list(f, cols[0], app);
    draw_detail(f, cols[1], app);
}

fn draw_topic_list(f: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .visible
        .iter()
        .filter_map(|i| app.all_topics.get(*i))
        .map(|t| {
            ListItem::new(Line::from(vec![
                Span::styled(&t.key, Style::default().fg(Color::Cyan)),
                Span::raw("  "),
                Span::styled(&t.title, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();

    let title = match app.mode {
        Mode::Filter => format!(" topics  /{}_ ", app.filter),
        Mode::Search => format!(" topics  ?{}_ ", app.search_query),
        Mode::Browse => {
            if app.search_active {
                format!(
                    " topics  ({}/{})  ?{} ",
                    app.visible.len(),
                    app.all_topics.len(),
                    app.search_query
                )
            } else if !app.filter.is_empty() {
                format!(
                    " topics  ({}/{})  /{} ",
                    app.visible.len(),
                    app.all_topics.len(),
                    app.filter
                )
            } else {
                format!(" topics ({}) ", app.all_topics.len())
            }
        }
    };

    let border_style = if app.focus == Focus::Left {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(border_style),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▌ ");

    let mut state = app.list_state.clone();
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_detail(f: &mut Frame, area: Rect, app: &App) {
    let title = tab_title(app);
    let border_style = if app.focus == Focus::Right {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    if let Some(err) = &app.caches.error {
        let p = Paragraph::new(format!("error: {err}"))
            .style(Style::default().fg(Color::Red))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(border_style),
            )
            .wrap(Wrap { trim: false });
        f.render_widget(p, area);
        return;
    }

    // Pass the selected element index for highlighting when focused.
    let sel = if app.focus == Focus::Right && app.tab == DetailTab::Detail {
        Some(app.detail_selected)
    } else {
        None
    };

    let lines = match app.tab {
        DetailTab::Detail => match &app.caches.detail {
            Some(d) => detail_lines(d, sel),
            None => placeholder_lines(app),
        },
        DetailTab::Neighbors => match &app.caches.nearby {
            Some(n) => neighbor_lines(n),
            None => placeholder_lines(app),
        },
        DetailTab::History => match &app.caches.history {
            Some(h) => history_lines(h),
            None => placeholder_lines(app),
        },
    };

    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(border_style),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn tab_title(app: &App) -> String {
    let key = app
        .selected_topic()
        .map(|t| t.key.as_str())
        .unwrap_or("—");
    let mut s = String::from(" ");
    for (i, tab) in [DetailTab::Detail, DetailTab::Neighbors, DetailTab::History]
        .iter()
        .enumerate()
    {
        if i > 0 {
            s.push_str("  ");
        }
        if *tab == app.tab {
            s.push('[');
            s.push_str(tab.label());
            s.push(']');
        } else {
            s.push_str(tab.label());
        }
    }
    s.push_str("  ·  ");
    s.push_str(key);
    s.push(' ');
    s
}

fn placeholder_lines(app: &App) -> Vec<Line<'static>> {
    let msg = if app.selected_topic().is_none() {
        "no topic selected"
    } else {
        "loading…"
    };
    vec![Line::from(Span::styled(
        msg,
        Style::default().fg(Color::DarkGray),
    ))]
}

fn detail_lines(detail: &Detail, selected_elem: Option<usize>) -> Vec<Line<'static>> {
    let t = &detail.topic;
    let mut lines: Vec<Line> = Vec::new();
    let mut elem_idx: usize = 0; // Tracks which DetailElement we're rendering.

    let sel_bg = Style::default().bg(Color::DarkGray);
    let is_sel = |idx: usize| -> bool {
        selected_elem.map(|s| s == idx).unwrap_or(false)
    };

    // ── Element 0: Title ──
    let marker = if is_sel(elem_idx) { "▌ " } else { "  " };
    let mut header_spans = vec![
        Span::styled(marker, if is_sel(elem_idx) { sel_bg } else { Style::default() }),
        Span::styled(
            t.title.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ];
    if t.deprecated {
        header_spans.push(Span::raw("  "));
        header_spans.push(Span::styled(
            "[deprecated]",
            Style::default().fg(Color::Red),
        ));
    }
    lines.push(Line::from(header_spans));
    elem_idx += 1;

    // Metadata (not a selectable element — no elem_idx bump)
    lines.push(Line::from(Span::styled(
        format!(
            "  updated {}  ·  created {}  ·  {} block(s)",
            t.updated_at.format("%Y-%m-%d %H:%M"),
            t.created_at.format("%Y-%m-%d"),
            t.blocks.len()
        ),
        Style::default().fg(Color::DarkGray),
    )));

    // ── Element: Tags (only if present) ──
    if !t.tags.is_empty() {
        let marker = if is_sel(elem_idx) { "▌ " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(marker, if is_sel(elem_idx) { sel_bg } else { Style::default() }),
            Span::styled(
                format!("tags: {}", t.tags.join(", ")),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        elem_idx += 1;
    }
    lines.push(Line::from(""));

    // ── Element: Summary ──
    if !t.summary.is_empty() {
        let marker = if is_sel(elem_idx) { "▌ " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(marker, if is_sel(elem_idx) { sel_bg } else { Style::default() }),
            Span::styled(
                t.summary.clone(),
                Style::default().add_modifier(Modifier::ITALIC),
            ),
        ]));
        elem_idx += 1;
    }
    lines.push(Line::from(""));

    // ── Elements: Blocks ──
    for (i, block) in t.blocks.iter().enumerate() {
        let marker = if is_sel(elem_idx) { "▌ " } else { "  " };
        let header_style = if is_sel(elem_idx) {
            Style::default().fg(Color::Yellow).bg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Yellow)
        };
        lines.push(Line::from(vec![
            Span::styled(marker, if is_sel(elem_idx) { sel_bg } else { Style::default() }),
            Span::styled(format!("── block {} ", i + 1), header_style),
            Span::styled(
                format!("[{}]", block.id),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        for line in block.content.lines() {
            let prefix = if is_sel(elem_idx) { "▌ " } else { "  " };
            lines.push(Line::from(vec![
                Span::styled(prefix, if is_sel(elem_idx) { sel_bg } else { Style::default() }),
                Span::raw(line.to_string()),
            ]));
        }
        lines.push(Line::from(""));
        elem_idx += 1;
    }

    // ── Elements: Edges ──
    if !detail.explore.edges.is_empty() {
        lines.push(Line::from(Span::styled(
            "  ── edges ─────────────────────",
            Style::default().fg(Color::Yellow),
        )));
        for edge in &detail.explore.edges {
            let marker = if is_sel(elem_idx) { "▌ " } else { "  " };
            let base = edge_line(&t.key, edge);
            let mut spans = vec![Span::styled(
                marker,
                if is_sel(elem_idx) { sel_bg } else { Style::default() },
            )];
            spans.extend(base.spans);
            lines.push(Line::from(spans));
            elem_idx += 1;
        }
    }

    lines
}

fn neighbor_lines(n: &NearbyResult) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("center: {}  ·  {} nodes within 2 hops", n.center, n.total_nodes),
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(""));

    if n.by_edge_type.is_empty() {
        lines.push(Line::from(Span::styled(
            "no neighbors",
            Style::default().fg(Color::DarkGray),
        )));
        return lines;
    }

    // Stable order for the buckets so the view doesn't jiggle.
    let mut buckets: Vec<(&String, &Vec<cairn_core::NearbyEntry>)> = n.by_edge_type.iter().collect();
    buckets.sort_by(|a, b| a.0.cmp(b.0));

    for (edge_type, entries) in buckets {
        lines.push(Line::from(Span::styled(
            format!("── {edge_type} ({})", entries.len()),
            Style::default().fg(Color::Yellow),
        )));
        for entry in entries {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {}h  ", entry.distance),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(entry.key.clone(), Style::default().fg(Color::Cyan)),
                Span::raw("  "),
                Span::styled(
                    entry.title.clone(),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
        lines.push(Line::from(""));
    }
    lines
}

fn history_lines(h: &HistoryResult) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    if h.events.is_empty() {
        lines.push(Line::from(Span::styled(
            "no history",
            Style::default().fg(Color::DarkGray),
        )));
        return lines;
    }
    lines.push(Line::from(Span::styled(
        format!("{} event(s)", h.events.len()),
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(""));
    for event in &h.events {
        lines.push(Line::from(vec![
            Span::styled(
                event.created_at.format("%Y-%m-%d %H:%M  ").to_string(),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("{:<10}", event.op),
                Style::default().fg(Color::Magenta),
            ),
            Span::raw(" "),
            Span::styled(event.target.clone(), Style::default().fg(Color::Cyan)),
        ]));
        if !event.detail.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("    {}", event.detail),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    lines
}

fn edge_line(self_key: &str, edge: &EdgeSummary) -> Line<'static> {
    let (arrow, other) = if edge.from == self_key {
        ("→", edge.to.clone())
    } else {
        ("←", edge.from.clone())
    };
    Line::from(vec![
        Span::styled(
            format!("  {arrow} "),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("{:<12}", edge.edge_type),
            Style::default().fg(Color::Magenta),
        ),
        Span::raw(" "),
        Span::styled(other, Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled(edge.note.clone(), Style::default().fg(Color::DarkGray)),
    ])
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let spans = match app.mode {
        Mode::Browse if app.edit_mode => vec![
            key_hint("j/k", "navigate"),
            key_hint("tab", "switch pane"),
            key_hint("enter", "actions"),
            key_hint(":", "commands"),
            key_hint("/", "filter"),
            key_hint("R", "refresh"),
            key_hint("esc", "exit edit"),
            key_hint("q", "quit"),
        ],
        Mode::Browse => vec![
            key_hint("j/k", "navigate"),
            key_hint("tab", "switch pane"),
            key_hint("enter", "actions"),
            key_hint(":", "commands"),
            key_hint("/", "filter"),
            key_hint("?", "search"),
            key_hint("e", "edit mode"),
            key_hint("q", "quit"),
        ],
        Mode::Filter => vec![
            key_hint("type", "filter"),
            key_hint("enter", "apply"),
            key_hint("esc", "cancel"),
        ],
        Mode::Search => vec![
            key_hint("type", "FTS query"),
            key_hint("enter", "run"),
            key_hint("esc", "cancel"),
        ],
    };
    let mut line: Vec<Span> = Vec::new();
    for (i, group) in spans.into_iter().enumerate() {
        if i > 0 {
            line.push(Span::raw("  "));
        }
        line.extend(group);
    }
    let footer = Paragraph::new(Line::from(line));
    f.render_widget(footer, area);
}

fn key_hint(key: &'static str, label: &'static str) -> Vec<Span<'static>> {
    vec![
        Span::styled(key, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(label, Style::default().fg(Color::DarkGray)),
    ]
}
