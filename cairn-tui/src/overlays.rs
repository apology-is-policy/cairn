use crate::handlers::Action;

/// Modal overlays that capture all key input while active. The main event
/// loop checks `app.overlay` first; when present, the overlay's key
/// handler runs and the normal key dispatch is suppressed. This is the
/// extension point for every dialog: edit confirmation, text input
/// prompts, the command palette, and anything else that needs focus.
///
/// Note: not Clone because `TextArea` carries undo history and cursor
/// state. `handle_overlay_key` uses `app.overlay.take()` to avoid borrow
/// conflicts, then puts the overlay back if it's still active.
pub enum Overlay {
    /// The "enter edit mode?" red confirmation dialog. Shown on `e` in
    /// browse mode. Enter confirms (acquires the lock), Esc cancels.
    /// If `pending_action` is set, that action is dispatched immediately
    /// after the lock is acquired — so pressing Enter on a block while
    /// not in edit mode flows directly into amend after confirming.
    EditConfirm { pending_action: Option<Action> },
    /// Notification toast — auto-dismissed on any keypress.
    Notification { message: String, is_error: bool },
    /// Fuzzy-filtered command palette (`:` in browse mode).
    CommandPalette { filter: String, selected: usize },
    /// Multiline text editor (tui-textarea). Used for editing block
    /// content, voice, edge notes, etc.
    /// Esc enters command mode (`:w` save, `:q` cancel, `:wq` save+close).
    /// The TextArea is boxed because it's much larger than the other
    /// variants (cursor state, undo history, line buffer).
    TextInput {
        title: String,
        textarea: Box<tui_textarea::TextArea<'static>>,
        purpose: TextInputPurpose,
        /// Vim-like editor mode.
        editor_mode: EditorMode,
        /// Original content for dirty-checking (`:q` warns if modified).
        original: String,
        /// True if `:w` was used on a non-terminal purpose (amend).
        pending_save: bool,
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
    /// Fuzzy-filtered topic picker for edge target selection.
    TopicPicker {
        filter: String,
        selected: usize,
        purpose: TopicPickerPurpose,
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
#[derive(Clone)]
pub enum TextInputPurpose {
    AmendBlock { topic_key: String, block_id: String },
    EditVoice,
    LearnContent { topic_key: String, title: String },
    AddBlockContent { topic_key: String },
    EditSummary { topic_key: String },
}

/// What the LineInput overlay should do with its content when confirmed.
#[allow(dead_code)]
pub enum LineInputPurpose {
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
    DeleteBlockReason {
        topic_key: String,
        block_id: String,
    },
}

/// Vim-like modal state for the TextInput editor.
#[derive(Clone)]
pub enum EditorMode {
    /// Normal mode: movement keys, `:` for commands, `i` for insert.
    Normal,
    /// Insert mode: all keys go to the textarea. Esc returns to Normal.
    Insert,
    /// Command mode (after `:` in Normal): typing a command like `w`, `q`, `wq`.
    Command(String),
}

pub enum TopicPickerPurpose {
    EdgeTarget { from_key: String },
}

#[derive(Clone)]
pub struct ContextMenuItem {
    pub label: String,
    pub action: Action,
}

/// Edge types for the picker. Matches cairn_core::EdgeKind::ALL.
pub const EDGE_TYPES: &[(&str, &str)] = &[
    ("depends_on", "A requires B to function"),
    ("gotcha", "B is a known pitfall when working with A"),
    ("war_story", "B is an incident related to A"),
    ("contradicts", "A and B contain conflicting information"),
    ("replaced_by", "A is outdated; B is current"),
    ("see_also", "Loose association"),
    ("owns", "Ownership / responsibility"),
];

/// Result of processing a key in an overlay context.
pub enum OverlayResult {
    /// The overlay consumed the key. The main event loop should `continue`.
    Consumed,
    /// The overlay produced an action to dispatch through the normal handler.
    Dispatch(Action),
}
