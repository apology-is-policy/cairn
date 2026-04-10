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
    default_db_path, CairnClient, EdgeSummary, ExploreParams, ExploreResult, GraphStatusResult,
    HistoryParams, HistoryResult, NearbyParams, NearbyResult, NodeSummary, SearchParams,
    SearchResult, SearchResultItem, Topic,
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

enum ListJump {
    First,
    Last,
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

        // Global quit chord works in any mode.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(());
        }

        let action = match app.mode {
            Mode::Browse => handle_browse_key(key.code, key.modifiers),
            Mode::Filter => handle_text_key(key.code, TextTarget::Filter),
            Mode::Search => handle_text_key(key.code, TextTarget::Search),
        };

        match action {
            Action::None => {}
            Action::Quit => return Ok(()),
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
        }
    }
}

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
}

#[derive(Clone, Copy)]
enum TextTarget {
    Filter,
    Search,
}

fn handle_browse_key(code: KeyCode, mods: KeyModifiers) -> Action {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
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
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let s = &app.status.stats;
    let header = Paragraph::new(Line::from(vec![
        Span::styled("cairn", Style::default().add_modifier(Modifier::BOLD)),
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
    ]))
    .block(Block::default().borders(Borders::ALL).title(" cairn-tui "));
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
        Mode::Browse => vec![
            key_hint("j/k", "move"),
            key_hint("h/l/tab", "tabs"),
            key_hint("1/2/3", "detail/neigh/hist"),
            key_hint("/", "filter"),
            key_hint("?", "search"),
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
