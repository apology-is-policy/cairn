use cairn_core::{EdgeSummary, HistoryResult, NearbyResult, NodeSummary};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};
use ratatui::Frame;

use crate::app::{App, Detail, DetailTab, Focus, Mode};
use crate::handlers::context_action_set;
use crate::handlers::scroll_offset;
use crate::overlays::{EditorMode, Overlay, EDGE_TYPES};
use crate::palette::filtered_palette;

// ── Rendering ─────────────────────────────────────────────────────

pub fn draw(f: &mut Frame, app: &App) {
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
pub fn soft_wrap(text: &str, width: usize) -> Vec<String> {
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
pub fn unwrap_soft(lines: &[String]) -> String {
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

/// Render a modal overlay centered on the screen.
pub fn draw_overlay(f: &mut Frame, app: &App, area: Rect) {
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
            let ctx = context_action_set(app);
            let matches = filtered_palette(filter, app.edit_mode, &ctx);
            let max_visible = 14usize;
            let list_height = matches.len().min(max_visible) as u16;
            let dialog_height = (list_height + 3).min(area.height.saturating_sub(4));
            let dialog_width = 80u16.min(area.width.saturating_sub(4));
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
            editor_mode,
            ..
        } => {
            let textarea = &**textarea_box;
            // Mode indicator + title
            let mode_label = match editor_mode {
                EditorMode::Normal => ("NOR", Color::Cyan),
                EditorMode::Insert => ("INS", Color::Green),
                EditorMode::Command(_) => ("CMD", Color::Yellow),
            };
            let margin = 2u16;
            let w = area.width.saturating_sub(margin * 2);
            let h = area.height.saturating_sub(margin * 2);
            let editor_area = Rect::new(margin, margin, w, h);
            f.render_widget(Clear, editor_area);

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(mode_label.1))
                .title(format!(" [{}] {} ", mode_label.0, title))
                .style(Style::default().bg(Color::Black));
            let inner = block.inner(editor_area);
            f.render_widget(block, editor_area);

            // Render the textarea.
            f.render_widget(textarea, inner);

            // Status / hint line at the bottom.
            if editor_area.height >= 3 {
                let hint_area = Rect::new(
                    editor_area.x + 1,
                    editor_area.y + editor_area.height - 1,
                    editor_area.width.saturating_sub(2),
                    1,
                );
                let hints = match editor_mode {
                    EditorMode::Normal => Line::from(vec![
                        Span::styled(
                            " NOR ",
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            "  i",
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" insert  ", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            ":",
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" command  ", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            "hjkl",
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" move", Style::default().fg(Color::DarkGray)),
                    ]),
                    EditorMode::Insert => Line::from(vec![
                        Span::styled(
                            " INS ",
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::Green)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            "  Esc",
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" normal mode", Style::default().fg(Color::DarkGray)),
                    ]),
                    EditorMode::Command(ref buf) => Line::from(vec![
                        Span::styled(
                            " CMD ",
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled("  ", Style::default()),
                        Span::styled(
                            buf.clone(),
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled("_", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            "   :w save  :q quit  :wq save+quit  Esc cancel",
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]),
                };
                f.render_widget(
                    Paragraph::new(hints).style(Style::default().bg(Color::Black)),
                    hint_area,
                );
            }
        }
        Overlay::LineInput { title, buffer, .. } => {
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
                let hint_area = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
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
        Overlay::TopicPicker {
            filter,
            selected,
            purpose: _,
        } => {
            let needle = filter.trim().to_lowercase();
            let filtered: Vec<&NodeSummary> = app
                .all_topics
                .iter()
                .filter(|t| {
                    needle.is_empty()
                        || t.key.to_lowercase().contains(&needle)
                        || t.title.to_lowercase().contains(&needle)
                })
                .collect();

            let max_visible = 14usize;
            let list_height = filtered.len().min(max_visible) as u16;
            let dialog_height = (list_height + 3).min(area.height.saturating_sub(4));
            let dialog_width = 70u16.min(area.width.saturating_sub(4));
            let x = (area.width.saturating_sub(dialog_width)) / 2;
            let y = (area.height.saturating_sub(dialog_height)) / 2;
            let dialog_area = Rect::new(x, y, dialog_width, dialog_height);

            f.render_widget(Clear, dialog_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Select target topic ")
                .style(Style::default().bg(Color::Black));
            let inner = block.inner(dialog_area);
            f.render_widget(block, dialog_area);

            if inner.height == 0 || inner.width == 0 {
                return;
            }

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

            let list_area = Rect::new(
                inner.x,
                inner.y + 1,
                inner.width,
                inner.height.saturating_sub(1),
            );

            let vh = list_area.height as usize;
            let scroll = scroll_offset(*selected, vh);
            let items: Vec<ListItem> = filtered
                .iter()
                .enumerate()
                .skip(scroll)
                .take(vh)
                .map(|(i, t)| {
                    let style = if i == *selected {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    let desc_style = if i == *selected {
                        Style::default().fg(Color::Black).bg(Color::Cyan)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(format!(" {:<30}", t.key), style),
                        Span::styled(&t.title, desc_style),
                    ]))
                })
                .collect();
            let list = List::new(items).style(Style::default().bg(Color::Black));
            f.render_widget(list, list_area);
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
                    ListItem::new(Line::from(Span::styled(format!(" {} ", item.label), style)))
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
                        Span::styled(note.chars().take(20).collect::<String>(), desc_style),
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

pub fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let s = &app.status.stats;
    let mut spans = vec![Span::styled(
        "cairn",
        Style::default().add_modifier(Modifier::BOLD),
    )];
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
        Span::styled(&app.status.db_path, Style::default().fg(Color::DarkGray)),
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

pub fn draw_body(f: &mut Frame, area: Rect, app: &App) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(area);

    draw_topic_list(f, cols[0], app);
    draw_detail(f, cols[1], app);
}

pub fn draw_topic_list(f: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .visible
        .iter()
        .filter_map(|i| app.all_topics.get(*i))
        .map(|t| {
            let (marker, key_color) = match t.tier {
                cairn_core::TopicTier::Atlas => ("", Color::Cyan),
                cairn_core::TopicTier::Journal => ("J ", Color::Blue),
                cairn_core::TopicTier::Notes => ("N ", Color::DarkGray),
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, Style::default().fg(key_color)),
                Span::styled(&t.key, Style::default().fg(key_color)),
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

pub fn draw_detail(f: &mut Frame, area: Rect, app: &App) {
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

    let (lines, elem_starts) = match app.tab {
        DetailTab::Detail => match &app.caches.detail {
            Some(d) => detail_lines(d, sel),
            None => (placeholder_lines(app), vec![]),
        },
        DetailTab::Neighbors => match &app.caches.nearby {
            Some(n) => (neighbor_lines(n), vec![]),
            None => (placeholder_lines(app), vec![]),
        },
        DetailTab::History => match &app.caches.history {
            Some(h) => (history_lines(h), vec![]),
            None => (placeholder_lines(app), vec![]),
        },
    };

    // Compute scroll offset so the selected element stays visible.
    let block_widget = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style);
    let inner_height = block_widget.inner(area).height as usize;
    let total_lines = lines.len();
    let scroll_y = if !elem_starts.is_empty() && app.tab == DetailTab::Detail {
        let elem_start = elem_starts.get(app.detail_selected).copied().unwrap_or(0);
        // Element end = next element's start, or total lines.
        let elem_end = elem_starts
            .get(app.detail_selected + 1)
            .copied()
            .unwrap_or(total_lines);
        // Scroll so that elem_end is visible (i.e. the whole block fits).
        let need_end_visible = scroll_offset(elem_end.saturating_sub(1), inner_height);
        // But don't scroll past the element start.
        let need_start_visible = elem_start;
        // Take the larger of the two — ensures the end is visible,
        // but if the element is taller than the viewport, show the start.
        need_end_visible.min(need_start_visible.max(need_end_visible)) as u16
    } else {
        0
    };

    let p = Paragraph::new(lines)
        .block(block_widget)
        .scroll((scroll_y, 0));
    f.render_widget(p, area);

    // Scrollbar (only when content overflows).
    if total_lines > inner_height {
        let mut scrollbar_state = ScrollbarState::new(total_lines).position(scroll_y as usize);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None);
        // Render inside the block's inner area (inside borders).
        let scrollbar_area = Rect::new(
            area.x + area.width.saturating_sub(1),
            area.y + 1,
            1,
            area.height.saturating_sub(2),
        );
        f.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }
}

pub fn tab_title(app: &App) -> String {
    let key = app.selected_topic().map(|t| t.key.as_str()).unwrap_or("—");
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

pub fn placeholder_lines(app: &App) -> Vec<Line<'static>> {
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

/// Returns (lines, element_start_lines) — the rendered lines and the
/// starting line number for each selectable DetailElement.
pub fn detail_lines(
    detail: &Detail,
    selected_elem: Option<usize>,
) -> (Vec<Line<'static>>, Vec<usize>) {
    let t = &detail.topic;
    let mut lines: Vec<Line> = Vec::new();
    let mut elem_idx: usize = 0;
    let mut elem_starts: Vec<usize> = Vec::new();

    let sel_bg = Style::default().bg(Color::DarkGray);
    let is_sel = |idx: usize| -> bool { selected_elem.map(|s| s == idx).unwrap_or(false) };

    // ── Element 0: Title ──
    elem_starts.push(lines.len());
    let marker = if is_sel(elem_idx) { "▌ " } else { "  " };
    let mut header_spans = vec![
        Span::styled(
            marker,
            if is_sel(elem_idx) {
                sel_bg
            } else {
                Style::default()
            },
        ),
        Span::styled(
            t.title.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ];
    if t.tier != cairn_core::TopicTier::Atlas {
        let tier_color = match t.tier {
            cairn_core::TopicTier::Journal => Color::Blue,
            cairn_core::TopicTier::Notes => Color::DarkGray,
            cairn_core::TopicTier::Atlas => Color::White,
        };
        header_spans.push(Span::raw("  "));
        header_spans.push(Span::styled(
            format!("[{}]", t.tier.label()),
            Style::default().fg(tier_color),
        ));
    }
    if t.locked {
        header_spans.push(Span::raw("  "));
        header_spans.push(Span::styled(
            "[locked]",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
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
        elem_starts.push(lines.len());
        let marker = if is_sel(elem_idx) { "▌ " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(
                marker,
                if is_sel(elem_idx) {
                    sel_bg
                } else {
                    Style::default()
                },
            ),
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
        elem_starts.push(lines.len());
        let marker = if is_sel(elem_idx) { "▌ " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(
                marker,
                if is_sel(elem_idx) {
                    sel_bg
                } else {
                    Style::default()
                },
            ),
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
        elem_starts.push(lines.len());
        let marker = if is_sel(elem_idx) { "▌ " } else { "  " };
        let header_style = if is_sel(elem_idx) {
            Style::default().fg(Color::Yellow).bg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Yellow)
        };
        lines.push(Line::from(vec![
            Span::styled(
                marker,
                if is_sel(elem_idx) {
                    sel_bg
                } else {
                    Style::default()
                },
            ),
            Span::styled(format!("── block {} ", i + 1), header_style),
            Span::styled(
                format!("[{}]", block.id),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        for line in block.content.lines() {
            let prefix = if is_sel(elem_idx) { "▌ " } else { "  " };
            lines.push(Line::from(vec![
                Span::styled(
                    prefix,
                    if is_sel(elem_idx) {
                        sel_bg
                    } else {
                        Style::default()
                    },
                ),
                Span::raw(line.to_string()),
            ]));
        }
        lines.push(Line::from(""));
        elem_idx += 1;
    }

    // ── Elements: Edges ──
    if !detail.explore.edges.is_empty() {
        // Edge section header (not a selectable element)
        lines.push(Line::from(Span::styled(
            "  ── edges ─────────────────────",
            Style::default().fg(Color::Yellow),
        )));
        for edge in &detail.explore.edges {
            elem_starts.push(lines.len());
            let marker = if is_sel(elem_idx) { "▌ " } else { "  " };
            let base = edge_line(&t.key, edge);
            let mut spans = vec![Span::styled(
                marker,
                if is_sel(elem_idx) {
                    sel_bg
                } else {
                    Style::default()
                },
            )];
            spans.extend(base.spans);
            lines.push(Line::from(spans));
            elem_idx += 1;
        }
    }

    (lines, elem_starts)
}

pub fn neighbor_lines(n: &NearbyResult) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(
            "center: {}  ·  {} nodes within 2 hops",
            n.center, n.total_nodes
        ),
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
    let mut buckets: Vec<(&String, &Vec<cairn_core::NearbyEntry>)> =
        n.by_edge_type.iter().collect();
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
                Span::styled(entry.title.clone(), Style::default().fg(Color::DarkGray)),
            ]));
        }
        lines.push(Line::from(""));
    }
    lines
}

pub fn history_lines(h: &HistoryResult) -> Vec<Line<'static>> {
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

pub fn edge_line(self_key: &str, edge: &EdgeSummary) -> Line<'static> {
    let (arrow, other) = if edge.from == self_key {
        ("→", edge.to.clone())
    } else {
        ("←", edge.from.clone())
    };
    Line::from(vec![
        Span::styled(format!("  {arrow} "), Style::default().fg(Color::DarkGray)),
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

pub fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let spans = match app.mode {
        Mode::Browse if app.edit_mode => vec![
            key_hint("j/k", "navigate"),
            key_hint("tab", "pane"),
            key_hint("S-tab", "right tab"),
            key_hint("enter", "actions"),
            key_hint(":", "commands"),
            key_hint("/", "filter"),
            key_hint("R", "refresh"),
            key_hint("esc", "exit edit"),
            key_hint("q", "quit"),
        ],
        Mode::Browse => vec![
            key_hint("j/k", "navigate"),
            key_hint("tab", "pane"),
            key_hint("S-tab", "right tab"),
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

pub fn key_hint(key: &'static str, label: &'static str) -> Vec<Span<'static>> {
    vec![
        Span::styled(key, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(label, Style::default().fg(Color::DarkGray)),
    ]
}
