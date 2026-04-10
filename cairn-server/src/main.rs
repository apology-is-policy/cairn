//! cairn-server — single-process daemon that owns the Cairn DB exclusively.
//!
//! Listens on a Unix socket. Every request is dispatched through one
//! `tokio::sync::Mutex<Cairn>`, so all operations are globally serialized.
//! This is the user-facing consistency property: while one client is `prime`-ing,
//! no other client can `learn`/`connect`/`amend`. Two MCP servers from two
//! Claude Code instances can both connect safely.

use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cairn_core::rpc::{CairnRequest, CairnResponse};
use cairn_core::{default_db_path, derive_lock_path, derive_socket_path, Cairn, CairnError};
use clap::Parser;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, Notify, Semaphore};

const MAX_IN_FLIGHT: usize = 1024;

#[derive(Parser)]
#[command(name = "cairn-server", about = "Cairn knowledge graph daemon")]
struct Args {
    /// Path to the Cairn database directory.
    #[arg(long, env = "CAIRN_DB")]
    db: Option<PathBuf>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let db_path = args.db.unwrap_or_else(default_db_path);
    if let Some(p) = db_path.parent() {
        std::fs::create_dir_all(p)?;
    }

    // 1. Acquire exclusive flock. Held for the entire process lifetime.
    let lock_path = derive_lock_path(&db_path);
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;

    if unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        tracing::info!(
            "another cairn-server is already holding the lock for {}",
            db_path.display()
        );
        // Exit cleanly so auto-spawn from clients doesn't error out.
        std::process::exit(0);
    }
    tracing::info!("acquired lock {}", lock_path.display());

    // 2. Open the Cairn facade. Creates the DB if missing.
    let cairn = Cairn::open(&db_path)
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let cairn = Arc::new(Mutex::new(cairn));

    // 3. Bind the Unix socket. Stale sockets are safe to remove now because
    //    the flock guarantees no other server is running.
    let socket_path = derive_socket_path(&db_path);
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }
    let listener = UnixListener::bind(&socket_path)?;
    tracing::info!("cairn-server listening on {}", socket_path.display());

    // 4. Shutdown signal.
    let shutdown = Arc::new(Notify::new());
    {
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            let mut sigint =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("failed to install SIGINT handler: {e}");
                        return;
                    }
                };
            let mut sigterm =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("failed to install SIGTERM handler: {e}");
                        return;
                    }
                };
            tokio::select! {
                _ = sigint.recv() => tracing::info!("received SIGINT"),
                _ = sigterm.recv() => tracing::info!("received SIGTERM"),
            }
            shutdown.notify_waiters();
        });
    }

    // 5. Accept loop.
    let in_flight = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
    loop {
        tokio::select! {
            _ = shutdown.notified() => {
                tracing::info!("shutdown requested, draining in-flight handlers");
                break;
            }
            accept = listener.accept() => {
                let (stream, _addr) = match accept {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("accept error: {e}");
                        continue;
                    }
                };
                let cairn = cairn.clone();
                let permit = match in_flight.clone().acquire_owned().await {
                    Ok(p) => p,
                    Err(_) => break,
                };
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, cairn).await {
                        tracing::debug!("connection ended: {e}");
                    }
                    drop(permit);
                });
            }
        }
    }

    // 6. Drain and clean up.
    drop(listener);
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        let _ = in_flight.acquire_many(MAX_IN_FLIGHT as u32).await;
    })
    .await;
    let _ = std::fs::remove_file(&socket_path);
    drop(lock_file);
    tracing::info!("cairn-server exited cleanly");
    Ok(())
}

async fn handle_connection(stream: UnixStream, cairn: Arc<Mutex<Cairn>>) -> std::io::Result<()> {
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(());
        }

        let req: CairnRequest = match serde_json::from_str(line.trim_end()) {
            Ok(r) => r,
            Err(e) => {
                let resp = CairnResponse {
                    ok: false,
                    result: None,
                    error: Some(format!("decode request: {e}")),
                    error_kind: Some("other".into()),
                };
                write_response(&mut w, &resp).await?;
                continue;
            }
        };

        let resp = {
            let guard = cairn.lock().await;
            dispatch(&guard, req).await
        };
        write_response(&mut w, &resp).await?;
    }
}

async fn write_response(w: &mut OwnedWriteHalf, resp: &CairnResponse) -> std::io::Result<()> {
    let mut buf = serde_json::to_vec(resp).map_err(std::io::Error::other)?;
    buf.push(b'\n');
    w.write_all(&buf).await?;
    w.flush().await
}

async fn dispatch(cairn: &Cairn, req: CairnRequest) -> CairnResponse {
    use CairnRequest::*;
    let result: Result<serde_json::Value, CairnError> = match req {
        Ping => Ok(serde_json::json!("pong")),
        SchemaVersion => cairn.schema_version().await.map(|v| serde_json::json!(v)),
        DbPath => Ok(serde_json::json!(cairn.db_path())),
        InitDefaults { initial_voice } => cairn
            .init_defaults(initial_voice.as_deref())
            .await
            .map(|_| serde_json::Value::Null),

        Learn(p) => to_val(cairn.learn(p).await),
        Connect(p) => to_val(cairn.connect(p).await),
        Amend(p) => to_val(cairn.amend(p).await),
        Forget(p) => to_val(cairn.forget(p).await),
        Rewrite(p) => to_val(cairn.rewrite(p).await),
        Rename(p) => to_val(cairn.rename(p).await),
        Reset => cairn.reset().await.map(|_| serde_json::Value::Null),
        Checkpoint(p) => to_val(cairn.checkpoint(p).await),
        History(p) => to_val(cairn.history(p).await),
        GetTopic { key } => to_val(cairn.get_topic(&key).await),

        Search(p) => to_val(cairn.search(p).await),
        Explore(p) => to_val(cairn.explore(p).await),
        Path(p) => to_val(cairn.path(p).await),
        Nearby(p) => to_val(cairn.nearby(p).await),
        Stats => to_val(cairn.stats().await),
        GraphView => to_val(cairn.graph_view().await),

        Prime(p) => to_val(cairn.prime(p).await),
        GraphStatus => to_val(cairn.graph_status().await),

        GetVoice => to_val(cairn.get_voice().await),
        SetVoice { content } => to_val(cairn.set_voice(&content).await),
        GetPreferences => to_val(cairn.get_preferences().await),
        SetPreferences { prefs } => cairn
            .set_preferences(&prefs)
            .await
            .map(|_| serde_json::Value::Null),

        Snapshot(p) => to_val(cairn.snapshot(p).await),
        Restore(p) => to_val(cairn.restore(p).await),
        ExportJson => cairn.export_json().await.map(|s| serde_json::json!(s)),
        ImportJson { json } => cairn
            .import_json(&json)
            .await
            .map(|(t, e)| serde_json::json!([t, e])),
        ListSnapshots => cairn.list_snapshots().map(|v| serde_json::json!(v)),
    };

    match result {
        Ok(v) => CairnResponse::ok_value(v),
        Err(e) => CairnResponse::err(&e),
    }
}

fn to_val<T: serde::Serialize>(r: Result<T, CairnError>) -> Result<serde_json::Value, CairnError> {
    r.map(|v| serde_json::to_value(v).unwrap_or(serde_json::Value::Null))
}
