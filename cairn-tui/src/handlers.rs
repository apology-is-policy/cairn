use cairn_core::CairnClient;
use crossterm::event::{self, KeyCode, KeyModifiers};
use ratatui::style::{Color, Style};

use crate::app::{App, DetailElement, DetailTab, Focus, ListJump, TopicCaches};
use crate::overlays::{
    ContextMenuItem, EditorMode, LineInputPurpose, Overlay, OverlayResult, TextInputPurpose,
    TopicPickerPurpose, EDGE_TYPES,
};
use crate::render::{soft_wrap, unwrap_soft};

#[derive(Clone, Copy)]
pub enum Action {
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
    /// Delete a block from the selected topic.
    DeleteBlock,
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
pub enum TextTarget {
    Filter,
    Search,
}

pub fn handle_browse_key(
    code: KeyCode,
    mods: KeyModifiers,
    edit_mode: bool,
    focus: Focus,
) -> Action {
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
        KeyCode::Char('D') if edit_mode => Action::DeleteBlock,
        KeyCode::Char('K') if edit_mode => Action::MoveBlockUp,
        KeyCode::Char('J') if edit_mode => Action::MoveBlockDown,
        KeyCode::Char('e') => Action::RequestEditMode,
        _ => Action::None,
    }
}

pub fn handle_text_key(code: KeyCode, _target: TextTarget) -> Action {
    match code {
        KeyCode::Esc => Action::ExitText,
        KeyCode::Enter => Action::ConfirmText,
        KeyCode::Backspace => Action::TextPop,
        KeyCode::Char(c) => Action::TextPush(c),
        _ => Action::None,
    }
}

/// Extract the Action values from the current context menu items,
/// used to prioritize them in the command palette.
pub fn context_action_set(app: &App) -> Vec<Action> {
    build_context_menu(app)
        .into_iter()
        .map(|i| i.action)
        .collect()
}

/// Build context-menu items based on the currently focused pane and
/// selected element. Returns an empty vec if nothing is actionable.
pub fn build_context_menu(app: &App) -> Vec<ContextMenuItem> {
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
            // Right pane: element-level actions. Shown even when not in
            // edit mode — the action handlers use require_edit_mode() to
            // prompt for the lock if needed, so pressing Enter on a block
            // in view mode flows into the edit confirm dialog then amend.
            if let Some(elem) = app.selected_detail_element() {
                match elem {
                    DetailElement::Title => {
                        items.push(ContextMenuItem {
                            label: "Rename topic".into(),
                            action: Action::RenameTopic,
                        });
                    }
                    DetailElement::Tags => {
                        items.push(ContextMenuItem {
                            label: "Edit tags".into(),
                            action: Action::EditTags,
                        });
                    }
                    DetailElement::Summary => {
                        items.push(ContextMenuItem {
                            label: "Edit summary".into(),
                            action: Action::EditSummary,
                        });
                    }
                    DetailElement::Block { .. } => {
                        items.push(ContextMenuItem {
                            label: "Amend this block".into(),
                            action: Action::AmendBlock,
                        });
                        items.push(ContextMenuItem {
                            label: "Add block".into(),
                            action: Action::AddBlock,
                        });
                        items.push(ContextMenuItem {
                            label: "Delete this block".into(),
                            action: Action::DeleteBlock,
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
                    DetailElement::Edge { .. } => {
                        items.push(ContextMenuItem {
                            label: "Remove this edge".into(),
                            action: Action::RemoveEdge,
                        });
                        items.push(ContextMenuItem {
                            label: "Add edge".into(),
                            action: Action::AddEdge,
                        });
                    }
                }
            }
        }
    }

    items
}

pub async fn handle_overlay_key(
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
                    Err(cairn_core::CairnError::EditorBusy { reason, since }) => {
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
        } => {
            let context_actions = context_action_set(app);
            match key.code {
                KeyCode::Esc => OverlayResult::Consumed,
                KeyCode::Enter => {
                    let matches =
                        crate::palette::filtered_palette(&filter, app.edit_mode, &context_actions);
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
                    let matches =
                        crate::palette::filtered_palette(&filter, app.edit_mode, &context_actions);
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
            }
        }
        Overlay::TextInput {
            textarea: mut textarea_box,
            title,
            purpose,
            editor_mode,
            original,
            pending_save,
        } => {
            let textarea = &mut *textarea_box;

            // Helper to put the overlay back with a given mode.
            macro_rules! stay {
                ($mode:expr) => {{
                    app.overlay = Some(Overlay::TextInput {
                        textarea: textarea_box,
                        title,
                        purpose,
                        editor_mode: $mode,
                        original,
                        pending_save,
                    });
                    OverlayResult::Consumed
                }};
            }

            match editor_mode {
                // ── NORMAL mode: commands, no text insertion ──
                EditorMode::Normal => match key.code {
                    KeyCode::Char('i') => stay!(EditorMode::Insert),
                    KeyCode::Char(':') => stay!(EditorMode::Command(":".into())),
                    // Movement in normal mode — pass to textarea
                    KeyCode::Char('h') | KeyCode::Left => {
                        textarea.input(crossterm::event::KeyEvent::new(
                            KeyCode::Left,
                            KeyModifiers::NONE,
                        ));
                        stay!(EditorMode::Normal)
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        textarea.input(crossterm::event::KeyEvent::new(
                            KeyCode::Down,
                            KeyModifiers::NONE,
                        ));
                        stay!(EditorMode::Normal)
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        textarea.input(crossterm::event::KeyEvent::new(
                            KeyCode::Up,
                            KeyModifiers::NONE,
                        ));
                        stay!(EditorMode::Normal)
                    }
                    KeyCode::Char('l') | KeyCode::Right => {
                        textarea.input(crossterm::event::KeyEvent::new(
                            KeyCode::Right,
                            KeyModifiers::NONE,
                        ));
                        stay!(EditorMode::Normal)
                    }
                    KeyCode::Char('0') | KeyCode::Home => {
                        textarea.input(crossterm::event::KeyEvent::new(
                            KeyCode::Home,
                            KeyModifiers::NONE,
                        ));
                        stay!(EditorMode::Normal)
                    }
                    KeyCode::Char('$') | KeyCode::End => {
                        textarea.input(crossterm::event::KeyEvent::new(
                            KeyCode::End,
                            KeyModifiers::NONE,
                        ));
                        stay!(EditorMode::Normal)
                    }
                    KeyCode::Char('g') => {
                        textarea.move_cursor(tui_textarea::CursorMove::Top);
                        stay!(EditorMode::Normal)
                    }
                    KeyCode::Char('G') => {
                        textarea.move_cursor(tui_textarea::CursorMove::Bottom);
                        stay!(EditorMode::Normal)
                    }
                    _ => stay!(EditorMode::Normal),
                },

                // ── INSERT mode: all keys to textarea, Esc → Normal ──
                EditorMode::Insert => {
                    if key.code == KeyCode::Esc {
                        stay!(EditorMode::Normal)
                    } else {
                        textarea.input(key);
                        stay!(EditorMode::Insert)
                    }
                }

                // ── COMMAND mode: `:` prompt at bottom ──
                EditorMode::Command(mut buf) => match key.code {
                    KeyCode::Esc => stay!(EditorMode::Normal),
                    KeyCode::Enter => {
                        let cmd = buf.trim_start_matches(':').trim().to_string();
                        match cmd.as_str() {
                            "wq" => {
                                let content = unwrap_soft(textarea.lines());
                                dispatch_text_save(app, client, purpose, content).await;
                                OverlayResult::Consumed
                            }
                            "w" => {
                                let content = unwrap_soft(textarea.lines());
                                let is_terminal =
                                    !matches!(purpose, TextInputPurpose::AmendBlock { .. });
                                if is_terminal {
                                    let purpose_clone = purpose.clone();
                                    dispatch_text_save(app, client, purpose, content.clone()).await;
                                    let lines = soft_wrap(&content, 76);
                                    let mut new_ta = tui_textarea::TextArea::new(lines);
                                    new_ta.set_cursor_line_style(Style::default());
                                    new_ta.set_style(Style::default().fg(Color::White));
                                    app.overlay = Some(Overlay::TextInput {
                                        title,
                                        textarea: Box::new(new_ta),
                                        purpose: purpose_clone,
                                        editor_mode: EditorMode::Normal,
                                        original: content,
                                        pending_save: false,
                                    });
                                } else {
                                    app.overlay = Some(Overlay::TextInput {
                                        textarea: textarea_box,
                                        title,
                                        purpose,
                                        editor_mode: EditorMode::Normal,
                                        original: content,
                                        pending_save: true,
                                    });
                                }
                                OverlayResult::Consumed
                            }
                            "q" => {
                                let current = unwrap_soft(textarea.lines());
                                if pending_save {
                                    dispatch_text_save(app, client, purpose, current).await;
                                    return OverlayResult::Consumed;
                                }
                                if current != original {
                                    // :q fails silently when dirty.
                                    stay!(EditorMode::Normal)
                                } else {
                                    OverlayResult::Consumed
                                }
                            }
                            "q!" => {
                                notify_ok(app, "Editor closed (changes discarded)".into());
                                OverlayResult::Consumed
                            }
                            _ => stay!(EditorMode::Command(format!("unknown: {cmd}"))),
                        }
                    }
                    KeyCode::Char(c) => {
                        buf.push(c);
                        stay!(EditorMode::Command(buf))
                    }
                    KeyCode::Backspace => {
                        buf.pop();
                        if buf.is_empty() {
                            stay!(EditorMode::Normal)
                        } else {
                            stay!(EditorMode::Command(buf))
                        }
                    }
                    _ => stay!(EditorMode::Command(buf)),
                },
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
                            topic_key,
                            block_id,
                            new_content,
                            reason: value,
                        })
                        .await
                    {
                        Ok(r) => {
                            notify_ok(
                                app,
                                format!("Amended block {} in '{}'", r.block_id, r.topic_key),
                            );
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
                            topic_key,
                            reason: value,
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
                            notify_ok(
                                app,
                                format!(
                                    "Checkpoint '{}' ({} mutations)",
                                    r.session_id, r.mutations_persisted
                                ),
                            );
                        }
                        Err(e) => notify_err(app, format!("Checkpoint failed: {e}")),
                    },
                    LineInputPurpose::NewTopicKey => {
                        // Chain: key → title prompt
                        app.overlay = Some(Overlay::LineInput {
                            title: format!("Title for '{}'", value),
                            buffer: String::new(),
                            purpose: LineInputPurpose::NewTopicTitle { topic_key: value },
                        });
                    }
                    LineInputPurpose::NewTopicTitle { topic_key } => {
                        // Chain: title → content editor
                        let mut textarea = tui_textarea::TextArea::new(vec![String::new()]);
                        textarea.set_cursor_line_style(Style::default());
                        textarea.set_style(Style::default().fg(Color::White));
                        app.overlay = Some(Overlay::TextInput {
                            title: format!("Content for '{}'", topic_key),
                            textarea: Box::new(textarea),
                            purpose: TextInputPurpose::LearnContent {
                                topic_key,
                                title: value,
                            },
                            editor_mode: EditorMode::Normal,
                            original: String::new(),
                            pending_save: false,
                        });
                    }
                    LineInputPurpose::DeleteBlockReason {
                        topic_key,
                        block_id,
                    } => match client
                        .delete_block(cairn_core::DeleteBlockParams {
                            topic_key,
                            block_id,
                            reason: value,
                        })
                        .await
                    {
                        Ok(r) => {
                            notify_ok(
                                app,
                                format!(
                                    "Deleted block {} ({} remaining)",
                                    r.block_id, r.remaining_blocks
                                ),
                            );
                            app.caches = TopicCaches::default();
                            app.fetch_active_tab(client).await;
                        }
                        Err(e) => notify_err(app, format!("Delete block failed: {e}")),
                    },
                    LineInputPurpose::EditSummary { topic_key } => match client
                        .set_summary(cairn_core::SetSummaryParams {
                            topic_key,
                            summary: value,
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
                                    notify_ok(
                                        app,
                                        format!("{} edge: {} → {}", r.action, r.from, r.to),
                                    );
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
        Overlay::TopicPicker {
            mut filter,
            mut selected,
            purpose,
        } => {
            // Compute filtered topics.
            let needle = filter.trim().to_lowercase();
            let filtered: Vec<&cairn_core::NodeSummary> = app
                .all_topics
                .iter()
                .filter(|t| {
                    needle.is_empty()
                        || t.key.to_lowercase().contains(&needle)
                        || t.title.to_lowercase().contains(&needle)
                })
                .collect();

            match key.code {
                KeyCode::Esc => OverlayResult::Consumed,
                KeyCode::Enter => {
                    if let Some(topic) = filtered.get(selected) {
                        let target_key = topic.key.clone();
                        match purpose {
                            TopicPickerPurpose::EdgeTarget { from_key } => {
                                app.overlay = Some(Overlay::EdgeTypePicker {
                                    from_key,
                                    to_key: target_key,
                                    selected: 0,
                                });
                            }
                        }
                    }
                    OverlayResult::Consumed
                }
                KeyCode::Char(c) => {
                    filter.push(c);
                    selected = 0;
                    app.overlay = Some(Overlay::TopicPicker {
                        filter,
                        selected,
                        purpose,
                    });
                    OverlayResult::Consumed
                }
                KeyCode::Backspace => {
                    filter.pop();
                    selected = 0;
                    app.overlay = Some(Overlay::TopicPicker {
                        filter,
                        selected,
                        purpose,
                    });
                    OverlayResult::Consumed
                }
                KeyCode::Down | KeyCode::Tab => {
                    if !filtered.is_empty() {
                        selected = (selected + 1).min(filtered.len() - 1);
                    }
                    app.overlay = Some(Overlay::TopicPicker {
                        filter,
                        selected,
                        purpose,
                    });
                    OverlayResult::Consumed
                }
                KeyCode::Up | KeyCode::BackTab => {
                    selected = selected.saturating_sub(1);
                    app.overlay = Some(Overlay::TopicPicker {
                        filter,
                        selected,
                        purpose,
                    });
                    OverlayResult::Consumed
                }
                _ => {
                    app.overlay = Some(Overlay::TopicPicker {
                        filter,
                        selected,
                        purpose,
                    });
                    OverlayResult::Consumed
                }
            }
        }
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
                    from_key,
                    to_key,
                    selected,
                });
                OverlayResult::Consumed
            }
            KeyCode::Up | KeyCode::Char('k') => {
                selected = selected.saturating_sub(1);
                app.overlay = Some(Overlay::EdgeTypePicker {
                    from_key,
                    to_key,
                    selected,
                });
                OverlayResult::Consumed
            }
            _ => {
                app.overlay = Some(Overlay::EdgeTypePicker {
                    from_key,
                    to_key,
                    selected,
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
                        if let Some(block) = detail.topic.blocks.iter().find(|b| b.id == *block_id)
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
                                editor_mode: EditorMode::Normal,
                                original: block.content.clone(),
                                pending_save: false,
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

/// Compute scroll offset so that `selected` is visible within `viewport_height` rows.
pub fn scroll_offset(selected: usize, viewport_height: usize) -> usize {
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
pub fn require_edit_mode(app: &mut App, pending: Action) -> bool {
    if app.edit_mode {
        false
    } else {
        app.overlay = Some(Overlay::EditConfirm {
            pending_action: Some(pending),
        });
        true
    }
}

/// Dispatch the save action for a TextInput overlay based on its purpose.
pub async fn dispatch_text_save(
    app: &mut App,
    client: &CairnClient,
    purpose: TextInputPurpose,
    content: String,
) {
    match purpose {
        TextInputPurpose::AmendBlock {
            topic_key,
            block_id,
        } => {
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
        TextInputPurpose::EditVoice => match client.set_voice(&content).await {
            Ok(_) => notify_ok(app, "Voice updated".into()),
            Err(e) => notify_err(app, format!("Set voice failed: {e}")),
        },
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
                    extra_blocks: vec![],
                })
                .await
            {
                Ok(r) => {
                    notify_ok(
                        app,
                        format!("Created topic '{}' (block {})", r.topic_key, r.block_id),
                    );
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
                        extra_blocks: vec![],
                    })
                    .await
                {
                    Ok(r) => {
                        notify_ok(
                            app,
                            format!("Added block {} to '{}'", r.block_id, r.topic_key),
                        );
                        app.caches = TopicCaches::default();
                        app.fetch_active_tab(client).await;
                    }
                    Err(e) => notify_err(app, format!("Add block failed: {e}")),
                }
            }
        }
        TextInputPurpose::EditSummary { topic_key } => {
            match client
                .set_summary(cairn_core::SetSummaryParams {
                    topic_key,
                    summary: content,
                })
                .await
            {
                Ok(r) => {
                    notify_ok(app, format!("Summary updated for '{}'", r.topic_key));
                    app.caches = TopicCaches::default();
                    app.fetch_active_tab(client).await;
                }
                Err(e) => notify_err(app, format!("Set summary failed: {e}")),
            }
        }
    }
}

pub fn notify_ok(app: &mut App, message: String) {
    app.overlay = Some(Overlay::Notification {
        message,
        is_error: false,
    });
}

pub fn notify_err(app: &mut App, message: String) {
    app.overlay = Some(Overlay::Notification {
        message,
        is_error: true,
    });
}
