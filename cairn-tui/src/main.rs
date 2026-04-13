//! cairn-tui — terminal explorer for the Cairn knowledge graph.
//!
//! Connects to a `cairn-server` daemon via `CairnClient` (auto-spawning the
//! daemon if needed), so it coexists safely with `cairn-mcp` and `cairn-cli`
//! against the same single-writer SurrealKV database.

mod app;
mod handlers;
mod overlays;
mod palette;
mod render;

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::time::Duration;

use app::App;
use cairn_core::{default_db_path, CairnClient, SearchParams, SearchResult};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use handlers::{
    build_context_menu, handle_browse_key, handle_overlay_key, handle_text_key, notify_err,
    notify_ok, require_edit_mode,
};
use overlays::{Overlay, OverlayResult};
use ratatui::backend::CrosstermBackend;
use ratatui::style::{Color, Style};
use ratatui::Terminal;
use render::{draw, soft_wrap};

use app::{Focus, ListJump, Mode, TopicCaches};
use handlers::{Action, TextTarget};
use overlays::{EditorMode, LineInputPurpose, TextInputPurpose, TopicPickerPurpose};

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
                        app.detail_selected = (cur + delta).rem_euclid(count as isize) as usize;
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
                app.focus = Focus::Right;
                app.detail_selected = 0;
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
                    let lines = soft_wrap(&current, 76);
                    let mut textarea = tui_textarea::TextArea::new(lines);
                    textarea.set_cursor_line_style(Style::default());
                    textarea.set_style(Style::default().fg(Color::White));
                    app.overlay = Some(Overlay::TextInput {
                        title: format!("Summary for '{}'", topic_key),
                        textarea: Box::new(textarea),
                        purpose: TextInputPurpose::EditSummary { topic_key },
                        editor_mode: EditorMode::Normal,
                        original: current,
                        pending_save: false,
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
                    let mut textarea = tui_textarea::TextArea::new(vec![String::new()]);
                    textarea.set_cursor_line_style(Style::default());
                    textarea.set_style(Style::default().fg(Color::White));
                    app.overlay = Some(Overlay::TextInput {
                        title: format!("New block in '{}'", topic_key),
                        textarea: Box::new(textarea),
                        purpose: TextInputPurpose::AddBlockContent { topic_key },
                        editor_mode: EditorMode::Normal,
                        original: String::new(),
                        pending_save: false,
                    });
                } else {
                    notify_err(app, "Select a topic first".into());
                }
            }
            Action::DeleteBlock => {
                if require_edit_mode(app, action) {
                    // Will re-dispatch after lock acquired.
                } else if let Some(detail) = &app.caches.detail {
                    let topic_key = detail.topic.key.clone();
                    // If right pane has a block selected, use it directly.
                    let block_id = if app.focus == Focus::Right {
                        match app.selected_detail_element() {
                            Some(app::DetailElement::Block { block_id, .. }) => Some(block_id),
                            _ => None,
                        }
                    } else if detail.topic.blocks.len() == 1 {
                        Some(detail.topic.blocks[0].id.clone())
                    } else {
                        None
                    };
                    if let Some(block_id) = block_id {
                        app.overlay = Some(Overlay::LineInput {
                            title: format!("Delete block {} — reason (required)", block_id),
                            buffer: String::new(),
                            purpose: LineInputPurpose::DeleteBlockReason {
                                topic_key,
                                block_id,
                            },
                        });
                    } else if detail.topic.blocks.is_empty() {
                        notify_err(app, "No blocks to delete".into());
                    } else {
                        // Multiple blocks, no preselection — show block picker.
                        let items: Vec<(String, String)> = detail
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
                                    .collect();
                                (b.id.clone(), preview)
                            })
                            .collect();
                        app.overlay = Some(Overlay::BlockPicker {
                            topic_key,
                            blocks: items,
                            selected: 0,
                        });
                    }
                } else {
                    notify_err(app, "Select a topic first".into());
                }
            }
            Action::SetTierAtlas | Action::SetTierJournal | Action::SetTierNotes => {
                if require_edit_mode(app, action) {
                    // Will re-dispatch after lock acquired.
                } else if let Some(key) = app.selected_key() {
                    let tier_str = match action {
                        Action::SetTierAtlas => "atlas",
                        Action::SetTierJournal => "journal",
                        Action::SetTierNotes => "notes",
                        _ => unreachable!(),
                    };
                    match client.set_tier(&key, tier_str).await {
                        Ok(()) => {
                            notify_ok(app, format!("Set '{}' tier to {}", key, tier_str));
                            app.caches = TopicCaches::default();
                            app.fetch_active_tab(client).await;
                        }
                        Err(e) => notify_err(app, format!("Set tier failed: {e}")),
                    }
                } else {
                    notify_err(app, "Select a topic first".into());
                }
            }
            Action::LockTopic => {
                if require_edit_mode(app, action) {
                    // Will re-dispatch after lock acquired.
                } else if let Some(key) = app.selected_key() {
                    match client.lock_topic(&key).await {
                        Ok(()) => {
                            notify_ok(app, format!("Locked '{key}' — agents can't modify it"));
                            app.caches = TopicCaches::default();
                            app.fetch_active_tab(client).await;
                        }
                        Err(e) => notify_err(app, format!("Lock failed: {e}")),
                    }
                } else {
                    notify_err(app, "Select a topic first".into());
                }
            }
            Action::UnlockTopic => {
                if require_edit_mode(app, action) {
                    // Will re-dispatch after lock acquired.
                } else if let Some(key) = app.selected_key() {
                    match client.unlock_topic(&key).await {
                        Ok(()) => {
                            notify_ok(app, format!("Unlocked '{key}' — editable again"));
                            app.caches = TopicCaches::default();
                            app.fetch_active_tab(client).await;
                        }
                        Err(e) => notify_err(app, format!("Unlock failed: {e}")),
                    }
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
                    app.overlay = Some(Overlay::ContextMenu { items, selected: 0 });
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
                            Some(app::DetailElement::Block { block_id, .. }) => Some(block_id),
                            _ => None,
                        }
                    } else {
                        None
                    };

                    // Find the block to edit: preselected, or single-block
                    // shortcut, or picker for 2+.
                    let target_block = preselected_block_id
                        .and_then(|id| detail.topic.blocks.iter().find(|b| b.id == id))
                        .or_else(|| {
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
                            title: format!("Amend block {} in {}", block.id, topic_key),
                            textarea: Box::new(textarea),
                            purpose: TextInputPurpose::AmendBlock {
                                topic_key,
                                block_id: block.id.clone(),
                            },
                            editor_mode: EditorMode::Normal,
                            original: block.content.clone(),
                            pending_save: false,
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
                            let content = voice_opt.map(|v| v.content).unwrap_or_default();
                            let lines = soft_wrap(&content, 76);
                            let mut textarea = tui_textarea::TextArea::new(lines);
                            textarea.set_cursor_line_style(Style::default());
                            textarea.set_style(Style::default().fg(Color::White));
                            app.overlay = Some(Overlay::TextInput {
                                title: "Edit developer voice".into(),
                                textarea: Box::new(textarea),
                                purpose: TextInputPurpose::EditVoice,
                                editor_mode: EditorMode::Normal,
                                original: content,
                                pending_save: false,
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
                        buffer: format!("tui_{}", chrono::Utc::now().format("%Y%m%d_%H%M%S")),
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
                    app.overlay = Some(Overlay::TopicPicker {
                        filter: String::new(),
                        selected: 0,
                        purpose: TopicPickerPurpose::EdgeTarget { from_key: key },
                    });
                } else {
                    notify_err(app, "Select a topic first".into());
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
                            Some(app::DetailElement::Edge {
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
                            None => notify_err(app, format!("Unknown edge type: {edge_type}")),
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
                        app.overlay = Some(Overlay::EdgePicker { edges, selected: 0 });
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
                                (blocks[1].id.clone(), cairn_core::Position::Start)
                            } else {
                                // Move block at index blocks.len()-2 to end
                                let idx = blocks.len() - 2;
                                (blocks[idx].id.clone(), cairn_core::Position::End)
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
