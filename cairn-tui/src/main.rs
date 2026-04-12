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
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
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
#[derive(Debug, Clone)]
enum Overlay {
    /// The "enter edit mode?" red confirmation dialog. Shown on `e` in
    /// browse mode. Enter confirms (acquires the lock), Esc cancels.
    EditConfirm,
    /// Notification toast — auto-dismissed on any keypress or after the
    /// next draw cycle if the user just hit a key. Used for transient
    /// success/error feedback.
    Notification {
        message: String,
        is_error: bool,
    },
    /// Fuzzy-filtered command palette (`:` in browse mode). Lists all
    /// available actions; the user types to narrow and Enter dispatches.
    CommandPalette {
        filter: String,
        selected: usize,
    },
}

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
            match handle_overlay_key(app, client, key.code).await {
                OverlayResult::Consumed => continue,
                OverlayResult::Dispatch(a) => dispatched_action = Some(a),
            }
        }

        // ── Normal key dispatch ──────────────────────────────────
        let action = dispatched_action.unwrap_or_else(|| match app.mode {
            Mode::Browse => handle_browse_key(key.code, key.modifiers, app.edit_mode),
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
            Action::Move(delta) => {
                app.move_selection(delta);
                app.on_selection_changed(client).await;
            }
            Action::Jump(j) => {
                app.jump_to(j);
                app.on_selection_changed(client).await;
            }
            Action::SwitchTab(t) => {
                app.tab = t;
                app.fetch_active_tab(client).await;
            }
            Action::NextTab => {
                app.tab = app.tab.next();
                app.fetch_active_tab(client).await;
            }
            Action::PrevTab => {
                app.tab = app.tab.prev();
                app.fetch_active_tab(client).await;
            }
            Action::RequestEditMode => {
                app.overlay = Some(Overlay::EditConfirm);
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
            Action::OpenPalette => {
                app.overlay = Some(Overlay::CommandPalette {
                    filter: String::new(),
                    selected: 0,
                });
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
    NextTab,
    PrevTab,
    /// Show the edit-mode confirmation dialog.
    RequestEditMode,
    /// Leave edit mode (release the daemon lock).
    ExitEditMode,
    /// Re-fetch all data from the daemon.
    Refresh,
    /// Open the command palette.
    OpenPalette,
}

#[derive(Clone, Copy)]
enum TextTarget {
    Filter,
    Search,
}

fn handle_browse_key(code: KeyCode, mods: KeyModifiers, edit_mode: bool) -> Action {
    match code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Esc if edit_mode => Action::ExitEditMode,
        KeyCode::Esc => Action::Quit,
        KeyCode::Char('j') | KeyCode::Down => Action::Move(1),
        KeyCode::Char('k') | KeyCode::Up => Action::Move(-1),
        KeyCode::Char('g') | KeyCode::Home => Action::Jump(ListJump::First),
        KeyCode::Char('G') | KeyCode::End => Action::Jump(ListJump::Last),
        KeyCode::Char('/') => Action::EnterFilter,
        KeyCode::Char('?') => Action::EnterSearch,
        KeyCode::Char('1') => Action::SwitchTab(DetailTab::Detail),
        KeyCode::Char('2') => Action::SwitchTab(DetailTab::Neighbors),
        KeyCode::Char('3') => Action::SwitchTab(DetailTab::History),
        KeyCode::Tab => Action::NextTab,
        KeyCode::BackTab => Action::PrevTab,
        KeyCode::Char('l') if !mods.contains(KeyModifiers::CONTROL) => Action::NextTab,
        KeyCode::Char('h') if !mods.contains(KeyModifiers::CONTROL) => Action::PrevTab,
        KeyCode::Char('e') if !edit_mode => Action::RequestEditMode,
        KeyCode::Char('R') => Action::Refresh,
        KeyCode::Char(':') => Action::OpenPalette,
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

async fn handle_overlay_key(
    app: &mut App,
    client: &CairnClient,
    code: KeyCode,
) -> OverlayResult {
    let overlay = app.overlay.clone().unwrap();
    match overlay {
        Overlay::EditConfirm => match code {
            KeyCode::Enter => {
                app.overlay = None;
                match client.begin_editor_session(Some("TUI edit session")).await {
                    Ok(()) => {
                        app.edit_mode = true;
                    }
                    Err(CairnError::EditorBusy { reason, since }) => {
                        let msg = format!(
                            "Editor lock held by another client since {} (reason: {})",
                            since.format("%H:%M:%S"),
                            reason.as_deref().unwrap_or("none")
                        );
                        app.overlay = Some(Overlay::Notification {
                            message: msg,
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
            KeyCode::Esc => {
                app.overlay = None;
                OverlayResult::Consumed
            }
            _ => OverlayResult::Consumed,
        },
        Overlay::Notification { .. } => {
            app.overlay = None;
            OverlayResult::Consumed
        }
        Overlay::CommandPalette { mut filter, mut selected } => match code {
            KeyCode::Esc => {
                app.overlay = None;
                OverlayResult::Consumed
            }
            KeyCode::Enter => {
                let matches = filtered_palette(&filter, app.edit_mode);
                if let Some((_, cmd)) = matches.get(selected) {
                    let action = cmd.action;
                    app.overlay = None;
                    OverlayResult::Dispatch(action)
                } else {
                    app.overlay = None;
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
            _ => OverlayResult::Consumed,
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

/// Render a modal overlay centered on the screen. Currently handles the
/// edit-mode confirmation dialog (red, prominent), notification toasts,
/// and the command palette.
fn draw_overlay(f: &mut Frame, app: &App, area: Rect) {
    let overlay = app.overlay.as_ref().unwrap();
    match overlay {
        Overlay::EditConfirm => {
            let dialog_width = 54u16.min(area.width.saturating_sub(4));
            let dialog_height = 9u16.min(area.height.saturating_sub(2));
            let x = (area.width.saturating_sub(dialog_width)) / 2;
            let y = (area.height.saturating_sub(dialog_height)) / 2;
            let dialog_area = Rect::new(x, y, dialog_width, dialog_height);

            // Clear the area under the dialog.
            let clear = Block::default().style(Style::default().bg(Color::Black));
            f.render_widget(clear, dialog_area);

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

            let items: Vec<ListItem> = matches
                .iter()
                .enumerate()
                .take(list_area.height as usize)
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
        Overlay::Notification { message, is_error } => {
            let color = if *is_error { Color::Red } else { Color::Green };
            let width = (message.len() as u16 + 6).min(area.width.saturating_sub(4));
            let x = (area.width.saturating_sub(width)) / 2;
            let y = area.height.saturating_sub(4);
            let note_area = Rect::new(x, y, width, 3);

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

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
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

    if let Some(err) = &app.caches.error {
        let p = Paragraph::new(format!("error: {err}"))
            .style(Style::default().fg(Color::Red))
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false });
        f.render_widget(p, area);
        return;
    }

    let lines = match app.tab {
        DetailTab::Detail => match &app.caches.detail {
            Some(d) => detail_lines(d),
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
        .block(Block::default().borders(Borders::ALL).title(title))
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

fn detail_lines(detail: &Detail) -> Vec<Line<'static>> {
    let t = &detail.topic;
    let mut lines: Vec<Line> = Vec::new();

    // Title line
    let mut header_spans = vec![Span::styled(
        t.title.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    )];
    if t.deprecated {
        header_spans.push(Span::raw("  "));
        header_spans.push(Span::styled(
            "[deprecated]",
            Style::default().fg(Color::Red),
        ));
    }
    lines.push(Line::from(header_spans));

    // Metadata
    lines.push(Line::from(Span::styled(
        format!(
            "updated {}  ·  created {}  ·  {} block(s)",
            t.updated_at.format("%Y-%m-%d %H:%M"),
            t.created_at.format("%Y-%m-%d"),
            t.blocks.len()
        ),
        Style::default().fg(Color::DarkGray),
    )));

    if !t.tags.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("tags: {}", t.tags.join(", ")),
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(""));

    // Summary
    lines.push(Line::from(Span::styled(
        t.summary.clone(),
        Style::default().add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(""));

    // Blocks
    for (i, block) in t.blocks.iter().enumerate() {
        lines.push(Line::from(vec![
            Span::styled(
                format!("── block {} ", i + 1),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!("[{}]", block.id),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        for line in block.content.lines() {
            lines.push(Line::from(line.to_string()));
        }
        lines.push(Line::from(""));
    }

    // Edges
    if !detail.explore.edges.is_empty() {
        lines.push(Line::from(Span::styled(
            "── edges ─────────────────────",
            Style::default().fg(Color::Yellow),
        )));
        for edge in &detail.explore.edges {
            lines.push(edge_line(&t.key, edge));
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
            key_hint("j/k", "move"),
            key_hint("h/l/tab", "tabs"),
            key_hint(":", "commands"),
            key_hint("/", "filter"),
            key_hint("?", "search"),
            key_hint("R", "refresh"),
            key_hint("esc", "exit edit"),
            key_hint("q", "quit"),
        ],
        Mode::Browse => vec![
            key_hint("j/k", "move"),
            key_hint("h/l/tab", "tabs"),
            key_hint(":", "commands"),
            key_hint("/", "filter"),
            key_hint("?", "search"),
            key_hint("e", "edit mode"),
            key_hint("R", "refresh"),
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
