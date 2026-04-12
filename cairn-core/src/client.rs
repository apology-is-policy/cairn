//! Cairn client — connects to a `cairn-server` daemon over a Unix socket.
//!
//! `CairnClient` mirrors every public method on `Cairn` so the binaries can
//! swap between in-process and client mode by changing only the constructor.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

use crate::error::{CairnError, Result};
use crate::rpc::{CairnRequest, CairnResponse};
use crate::types::*;

// ── Path helpers (shared with cairn-server) ──────────────────────

/// Compute the Unix socket path for a given DB path.
///
/// The DB path is a directory (SurrealKV stores multiple files in it). The
/// socket lives next to the DB directory, named `<stem>.sock`.
///
/// Example: `~/.cairn/cairn.db/` → `~/.cairn/cairn.sock`
pub fn derive_socket_path(db_path: &Path) -> PathBuf {
    let parent = db_path.parent().unwrap_or(Path::new("."));
    let stem = db_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("cairn");
    parent.join(format!("{stem}.sock"))
}

/// Compute the lock file path used to enforce single-server-per-DB.
///
/// Example: `~/.cairn/cairn.db/` → `~/.cairn/.cairn.db.lock`
pub fn derive_lock_path(db_path: &Path) -> PathBuf {
    let parent = db_path.parent().unwrap_or(Path::new("."));
    let name = db_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("cairn.db");
    parent.join(format!(".{name}.lock"))
}

// ── Client ───────────────────────────────────────────────────────

struct Connection {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    line_buf: String,
}

pub struct CairnClient {
    inner: Mutex<Connection>,
    db_path: String,
    socket_path: PathBuf,
}

impl CairnClient {
    /// Connect to a running `cairn-server` for the given DB. Does not spawn.
    pub async fn connect(db_path: &Path) -> Result<Self> {
        let socket_path = derive_socket_path(db_path);
        let stream = UnixStream::connect(&socket_path)
            .await
            .map_err(|e| CairnError::Other(format!("connect to cairn-server: {e}")))?;
        Ok(Self::from_stream(stream, db_path, socket_path))
    }

    /// Connect to `cairn-server`, auto-spawning it if needed.
    pub async fn connect_or_spawn(db_path: &Path) -> Result<Self> {
        let socket_path = derive_socket_path(db_path);
        let stream = open_stream_or_spawn(db_path, &socket_path).await?;
        Ok(Self::from_stream(stream, db_path, socket_path))
    }

    /// Re-establish the connection in place after a connection-level failure.
    ///
    /// Used by `call()` when the cached socket is dead — typically because
    /// the daemon was restarted (e.g. by `install.sh` after an upgrade).
    /// Auto-spawns a new daemon if no socket is reachable. The new
    /// `Connection` replaces the old one inside `self.inner`, so any other
    /// in-flight callers waiting on the lock pick up the fresh socket
    /// transparently.
    async fn reconnect(&self) -> Result<()> {
        let stream = open_stream_or_spawn(Path::new(&self.db_path), &self.socket_path).await?;
        let (r, w) = stream.into_split();
        let mut conn = self.inner.lock().await;
        conn.reader = BufReader::new(r);
        conn.writer = w;
        conn.line_buf.clear();
        Ok(())
    }

    fn from_stream(stream: UnixStream, db_path: &Path, socket_path: PathBuf) -> Self {
        let (r, w) = stream.into_split();
        Self {
            inner: Mutex::new(Connection {
                reader: BufReader::new(r),
                writer: w,
                line_buf: String::new(),
            }),
            db_path: db_path.display().to_string(),
            socket_path,
        }
    }

    /// The DB path this client targets (used for logging/diagnostics).
    pub fn db_path(&self) -> &str {
        &self.db_path
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    async fn call<T: DeserializeOwned>(&self, req: CairnRequest) -> Result<T> {
        let line =
            serde_json::to_string(&req).map_err(|e| CairnError::Other(format!("encode: {e}")))?;

        // First attempt with the cached connection.
        match self.try_call::<T>(&line).await {
            Ok(v) => Ok(v),
            Err(e) if e.is_connection_dead() => {
                // The daemon went away (most likely a planned restart from
                // `install.sh`). Reconnect once and retry. The reconnect
                // will auto-spawn a fresh daemon if needed.
                self.reconnect().await?;
                self.try_call::<T>(&line)
                    .await
                    .map_err(CallError::into_cairn_error)
            }
            Err(e) => Err(e.into_cairn_error()),
        }
    }

    async fn try_call<T: DeserializeOwned>(
        &self,
        line: &str,
    ) -> std::result::Result<T, CallError> {
        let mut conn = self.inner.lock().await;

        conn.writer
            .write_all(line.as_bytes())
            .await
            .map_err(CallError::Connection)?;
        conn.writer
            .write_all(b"\n")
            .await
            .map_err(CallError::Connection)?;
        conn.writer.flush().await.map_err(CallError::Connection)?;

        let Connection {
            reader, line_buf, ..
        } = &mut *conn;
        line_buf.clear();
        let n = reader
            .read_line(line_buf)
            .await
            .map_err(CallError::Connection)?;
        if n == 0 {
            return Err(CallError::UnexpectedEof);
        }

        let resp: CairnResponse = serde_json::from_str(line_buf.trim_end())
            .map_err(|e| CallError::Codec(format!("decode envelope: {e}")))?;

        if !resp.ok {
            return Err(CallError::Remote(reconstruct_error(resp)));
        }
        let value = resp.result.unwrap_or(serde_json::Value::Null);
        serde_json::from_value(value)
            .map_err(|e| CallError::Codec(format!("decode result: {e}")))
    }

    // ── Mirrored Cairn API ───────────────────────────────────────

    pub async fn schema_version(&self) -> Result<i64> {
        self.call(CairnRequest::SchemaVersion).await
    }

    pub async fn init_defaults(&self, initial_voice: Option<&str>) -> Result<()> {
        self.call(CairnRequest::InitDefaults {
            initial_voice: initial_voice.map(String::from),
        })
        .await
    }

    pub async fn learn(&self, params: LearnParams) -> Result<LearnResult> {
        self.call(CairnRequest::Learn(params)).await
    }

    pub async fn connect_topics(&self, params: ConnectParams) -> Result<ConnectResult> {
        self.call(CairnRequest::Connect(params)).await
    }

    pub async fn amend(&self, params: AmendParams) -> Result<AmendResult> {
        self.call(CairnRequest::Amend(params)).await
    }

    pub async fn forget(&self, params: ForgetParams) -> Result<ForgetResult> {
        self.call(CairnRequest::Forget(params)).await
    }

    pub async fn rewrite(&self, params: RewriteParams) -> Result<RewriteResult> {
        self.call(CairnRequest::Rewrite(params)).await
    }

    pub async fn rename(&self, params: RenameParams) -> Result<RenameResult> {
        self.call(CairnRequest::Rename(params)).await
    }

    pub async fn reset(&self) -> Result<()> {
        self.call(CairnRequest::Reset).await
    }

    pub async fn checkpoint(&self, params: CheckpointParams) -> Result<CheckpointResult> {
        self.call(CairnRequest::Checkpoint(params)).await
    }

    pub async fn history(&self, params: HistoryParams) -> Result<HistoryResult> {
        self.call(CairnRequest::History(params)).await
    }

    pub async fn get_topic(&self, key: &str) -> Result<Topic> {
        self.call(CairnRequest::GetTopic {
            key: key.to_string(),
        })
        .await
    }

    pub async fn search(&self, params: SearchParams) -> Result<SearchResult> {
        self.call(CairnRequest::Search(params)).await
    }

    pub async fn explore(&self, params: ExploreParams) -> Result<ExploreResult> {
        self.call(CairnRequest::Explore(params)).await
    }

    pub async fn path(&self, params: PathParams) -> Result<PathResult> {
        self.call(CairnRequest::Path(params)).await
    }

    pub async fn nearby(&self, params: NearbyParams) -> Result<NearbyResult> {
        self.call(CairnRequest::Nearby(params)).await
    }

    pub async fn stats(&self) -> Result<StatsResult> {
        self.call(CairnRequest::Stats).await
    }

    pub async fn graph_view(&self) -> Result<GraphViewResult> {
        self.call(CairnRequest::GraphView).await
    }

    pub async fn prime(&self, params: PrimeParams) -> Result<PrimeResult> {
        self.call(CairnRequest::Prime(params)).await
    }

    pub async fn graph_status(&self) -> Result<GraphStatusResult> {
        self.call(CairnRequest::GraphStatus).await
    }

    pub async fn get_voice(&self) -> Result<Option<Voice>> {
        self.call(CairnRequest::GetVoice).await
    }

    pub async fn set_voice(&self, content: &str) -> Result<VoiceResult> {
        self.call(CairnRequest::SetVoice {
            content: content.to_string(),
        })
        .await
    }

    pub async fn get_preferences(&self) -> Result<Preferences> {
        self.call(CairnRequest::GetPreferences).await
    }

    pub async fn set_preferences(&self, prefs: &Preferences) -> Result<()> {
        self.call(CairnRequest::SetPreferences {
            prefs: prefs.clone(),
        })
        .await
    }

    pub async fn snapshot(&self, params: SnapshotParams) -> Result<SnapshotResult> {
        self.call(CairnRequest::Snapshot(params)).await
    }

    pub async fn restore(&self, params: RestoreParams) -> Result<RestoreResult> {
        self.call(CairnRequest::Restore(params)).await
    }

    pub async fn export_json(&self) -> Result<String> {
        self.call(CairnRequest::ExportJson).await
    }

    pub async fn import_json(&self, json: &str) -> Result<(usize, usize)> {
        self.call(CairnRequest::ImportJson {
            json: json.to_string(),
        })
        .await
    }

    pub async fn list_snapshots(&self) -> Result<Vec<SnapshotResult>> {
        self.call(CairnRequest::ListSnapshots).await
    }

    // ── New ops (v4) ──────────────────────────────────────────────

    pub async fn set_summary(&self, params: SetSummaryParams) -> Result<SetSummaryResult> {
        self.call(CairnRequest::SetSummary(params)).await
    }

    pub async fn set_tags(&self, params: SetTagsParams) -> Result<SetTagsResult> {
        self.call(CairnRequest::SetTags(params)).await
    }

    pub async fn delete_block(&self, params: DeleteBlockParams) -> Result<DeleteBlockResult> {
        self.call(CairnRequest::DeleteBlock(params)).await
    }

    pub async fn disconnect(&self, params: DisconnectParams) -> Result<DisconnectResult> {
        self.call(CairnRequest::Disconnect(params)).await
    }

    pub async fn move_block(&self, params: MoveBlockParams) -> Result<MoveBlockResult> {
        self.call(CairnRequest::MoveBlock(params)).await
    }

    // ── Editor session control (v3) ──────────────────────────────

    /// Acquire the exclusive editor lock on the daemon. While this client
    /// holds the lock, *other* clients receive `CairnError::EditorBusy` on
    /// any mutation, but reads stay available. Calling `begin_editor_session`
    /// from a connection that already holds the lock is idempotent (the
    /// reason is updated, no error). The lock is released by
    /// `end_editor_session()` or automatically when this connection drops.
    pub async fn begin_editor_session(&self, reason: Option<&str>) -> Result<()> {
        self.call(CairnRequest::BeginEditorSession(BeginEditorSessionParams {
            reason: reason.map(String::from),
        }))
        .await
    }

    /// Release the editor lock if held by this connection. No-op if this
    /// connection isn't the holder, so it's safe to call defensively on
    /// shutdown paths.
    pub async fn end_editor_session(&self) -> Result<()> {
        self.call(CairnRequest::EndEditorSession).await
    }

    /// Inspect who currently holds the editor lock, if anyone. Useful for
    /// the TUI to show a "lock held by another session since X, reason Y"
    /// message before attempting to acquire it.
    pub async fn editor_session_status(&self) -> Result<Option<EditorSessionInfo>> {
        self.call(CairnRequest::EditorSessionStatus).await
    }
}

// ── Auto-spawn ───────────────────────────────────────────────────

/// Try to open a Unix-socket connection to a running cairn-server. If none
/// is reachable, spawn one and poll for it. Used by both `connect_or_spawn`
/// (initial connect) and `reconnect` (after a connection-level failure).
async fn open_stream_or_spawn(db_path: &Path, socket_path: &Path) -> Result<UnixStream> {
    // Fast path: existing daemon.
    if let Ok(stream) = UnixStream::connect(socket_path).await {
        return Ok(stream);
    }

    // Slow path: spawn detached and poll for the socket to appear.
    if let Some(parent) = db_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    spawn_server(db_path)?;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Ok(stream) = UnixStream::connect(socket_path).await {
            return Ok(stream);
        }
        if Instant::now() >= deadline {
            return Err(CairnError::Other(
                "cairn-server did not become reachable within 5 seconds. \
                 Check ~/.cairn/logs/cairn-server.log for details."
                    .into(),
            ));
        }
    }
}

fn spawn_server(db_path: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let bin = find_server_binary()?;

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let log_dir = PathBuf::from(&home).join(".cairn").join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("cairn-server.log");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| CairnError::Other(format!("open log file: {e}")))?;

    let mut cmd = Command::new(&bin);
    cmd.arg("--db")
        .arg(db_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log_file));

    // Detach from the parent so the daemon survives if the parent exits.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    cmd.spawn()
        .map_err(|e| CairnError::Other(format!("spawn cairn-server: {e}")))?;

    Ok(())
}

fn find_server_binary() -> Result<PathBuf> {
    if let Some(path) = which("cairn-server") {
        return Ok(path);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let fallback = PathBuf::from(home).join(".local/bin/cairn-server");
    if fallback.exists() {
        return Ok(fallback);
    }
    Err(CairnError::Other(
        "cairn-server binary not found on PATH or in ~/.local/bin. \
         Install with ./install.sh"
            .into(),
    ))
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// ── Internal call-error classification ───────────────────────────

/// Internal error type used by `try_call`. Captures enough detail to
/// decide whether the failure is "the connection is dead, retry safely"
/// or "the request was processed, surface this to the caller".
enum CallError {
    /// `write`/`read` on the socket failed. The `io::ErrorKind` decides
    /// whether the connection is dead and retry is safe.
    Connection(std::io::Error),
    /// `read_line` returned 0 bytes — the daemon closed the connection
    /// without sending a response. With cairn-server's clean SIGTERM
    /// handling this only happens when the daemon shut down between
    /// requests, so retry is safe.
    UnexpectedEof,
    /// The daemon returned a structured error response. Pass through
    /// without retrying.
    Remote(CairnError),
    /// Local encode/decode error. Pass through without retrying.
    Codec(String),
}

impl CallError {
    /// True if the failure means the cached connection is unusable and
    /// the request was (almost certainly) never processed by the daemon,
    /// so a retry on a fresh connection is safe.
    fn is_connection_dead(&self) -> bool {
        use std::io::ErrorKind::*;
        match self {
            CallError::Connection(e) => matches!(
                e.kind(),
                BrokenPipe | ConnectionReset | ConnectionAborted | NotConnected | UnexpectedEof
            ),
            CallError::UnexpectedEof => true,
            CallError::Remote(_) | CallError::Codec(_) => false,
        }
    }

    fn into_cairn_error(self) -> CairnError {
        match self {
            CallError::Connection(e) => CairnError::Other(format!("cairn-server: {e}")),
            CallError::UnexpectedEof => {
                CairnError::Other("cairn-server closed the connection unexpectedly".into())
            }
            CallError::Remote(e) => e,
            CallError::Codec(msg) => CairnError::Other(msg),
        }
    }
}

// ── Error reconstruction ─────────────────────────────────────────

fn reconstruct_error(resp: CairnResponse) -> CairnError {
    let kind = resp.error_kind.clone();
    let data = resp.error_data.clone();
    let msg = resp.error.unwrap_or_default();
    match kind.as_deref() {
        Some("topic_not_found") => CairnError::TopicNotFound(msg),
        Some("snapshot_not_found") => CairnError::SnapshotNotFound(msg),
        Some("invalid_edge_type") => CairnError::InvalidEdgeType(msg),
        Some("empty_content") => CairnError::EmptyContent(msg),
        Some("topic_key_conflict") => CairnError::TopicKeyConflict(msg),
        Some("db") => CairnError::Db(msg),
        Some("editor_busy") => {
            // Reconstruct from the structured payload if it's there.
            // If it isn't (e.g. talking to a daemon that for some reason
            // didn't include it), fall back to Other so the user still
            // sees the message.
            if let Some(d) = data {
                if let (Some(since), reason) = (
                    d.get("since")
                        .and_then(|v| serde_json::from_value(v.clone()).ok()),
                    d.get("reason")
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or(None),
                ) {
                    return CairnError::EditorBusy { since, reason };
                }
            }
            CairnError::Other(msg)
        }
        // BlockNotFound and SchemaVersionMismatch carry multiple fields that
        // don't survive the simple wire format. Display string is preserved.
        _ => CairnError::Other(msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_derivation() {
        let p = derive_socket_path(Path::new("/home/u/.cairn/cairn.db"));
        assert_eq!(p, PathBuf::from("/home/u/.cairn/cairn.sock"));
    }

    #[test]
    fn lock_path_derivation() {
        let p = derive_lock_path(Path::new("/home/u/.cairn/cairn.db"));
        assert_eq!(p, PathBuf::from("/home/u/.cairn/.cairn.db.lock"));
    }

    #[test]
    fn socket_path_handles_custom_names() {
        let p = derive_socket_path(Path::new("/tmp/work.db"));
        assert_eq!(p, PathBuf::from("/tmp/work.sock"));
    }
}
