use chrono::{DateTime, Utc};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CairnError {
    #[error("Database error: {0}")]
    Db(String),

    #[error("Topic not found: {0}")]
    TopicNotFound(String),

    #[error("Block not found: {0} in topic {1}")]
    BlockNotFound(String, String),

    #[error("Snapshot not found: {0}")]
    SnapshotNotFound(String),

    #[error("Invalid edge type: {0}")]
    InvalidEdgeType(String),

    #[error("Empty content: {0}")]
    EmptyContent(String),

    #[error("Topic key already exists: {0}")]
    TopicKeyConflict(String),

    #[error("Schema version mismatch: database is at v{db}, binary supports v{binary}. Update the binary to a newer version.")]
    SchemaVersionMismatch { db: i64, binary: i64 },

    #[error("Topic is locked: {0}. This topic has been curated by the user and is read-only. Use `unlock` to make it editable again.")]
    TopicLocked(String),

    /// A mutation was attempted while another client holds an exclusive
    /// editor session on the daemon. Reads (`prime`, `search`, `stats`,
    /// `graph_status`, …) bypass the lock and stay available.
    #[error("Editor session is held by another client (since {since}{}). Mutations are blocked until that client releases the lock or disconnects.", reason.as_deref().map(|r| format!(", reason: {r}")).unwrap_or_default())]
    EditorBusy {
        since: DateTime<Utc>,
        reason: Option<String>,
    },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, CairnError>;
