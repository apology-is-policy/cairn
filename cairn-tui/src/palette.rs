use crate::app::DetailTab;
use crate::handlers::Action;

// ── Command palette commands ──────────────────────────────────────

pub struct PaletteCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub key_hint: Option<&'static str>,
    pub action: Action,
    /// Only shown when edit_mode is true.
    pub edit_only: bool,
    /// Hidden when edit_mode is true (e.g. "Enter edit mode").
    pub browse_only: bool,
}

pub fn all_palette_commands() -> Vec<PaletteCommand> {
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
            name: "Delete block",
            description: "Remove a block from the selected topic",
            key_hint: Some("D"),
            action: Action::DeleteBlock,
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

pub fn filtered_palette(
    filter: &str,
    edit_mode: bool,
    context_actions: &[Action],
) -> Vec<(usize, &'static PaletteCommand)> {
    use std::sync::OnceLock;
    static COMMANDS: OnceLock<Vec<PaletteCommand>> = OnceLock::new();
    let commands = COMMANDS.get_or_init(all_palette_commands);

    let needle = filter.trim().to_lowercase();
    let mut matched: Vec<(usize, &PaletteCommand)> = commands
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
        .collect();

    // Partition: context-relevant actions first, then the rest.
    if !context_actions.is_empty() && needle.is_empty() {
        matched.sort_by_key(|(_, cmd)| {
            // Actions in context_actions sort to the front (key=0), rest to back (key=1).
            let dominated = |a: &Action, b: &Action| -> bool {
                std::mem::discriminant(a) == std::mem::discriminant(b)
            };
            if context_actions.iter().any(|ca| dominated(ca, &cmd.action)) {
                0
            } else {
                1
            }
        });
    }

    matched
}
