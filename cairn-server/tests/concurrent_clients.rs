//! Integration tests: spawn cairn-server, drive it via CairnClient, verify
//! that multiple clients see consistent state and that a second server refuses
//! to start cleanly.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use cairn_core::*;

fn server_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cairn-server")
}

fn temp_db() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cairn-it-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("cairn.db")
}

async fn wait_for_socket(db_path: &std::path::Path) {
    let socket = derive_socket_path(db_path);
    for _ in 0..50 {
        if socket.exists() {
            // give the server a moment to actually accept on it
            tokio::time::sleep(Duration::from_millis(50)).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("server socket never appeared at {}", socket.display());
}

struct ServerGuard {
    child: Option<std::process::Child>,
    db_dir: PathBuf,
}

impl ServerGuard {
    /// Kill and reap the daemon now, but leave the DB directory intact.
    /// Used by tests that simulate an in-place daemon restart.
    fn kill_now(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.db_dir);
    }
}

async fn spawn_server(db_path: &std::path::Path) -> ServerGuard {
    let child = Command::new(server_bin())
        .arg("--db")
        .arg(db_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn cairn-server");
    wait_for_socket(db_path).await;
    ServerGuard {
        child: Some(child),
        db_dir: db_path.parent().unwrap().to_path_buf(),
    }
}

#[tokio::test]
async fn two_clients_interleave_safely() {
    let db = temp_db();
    let _server = spawn_server(&db).await;

    let c1 = CairnClient::connect(&db).await.expect("client 1");
    let c2 = CairnClient::connect(&db).await.expect("client 2");

    c1.init_defaults(Some("test voice")).await.unwrap();

    c1.learn(LearnParams {
        topic_key: "alpha".into(),
        title: Some("Alpha".into()),
        summary: Some("first".into()),
        content: "content alpha".into(),
        voice: None,
        tags: vec!["a".into()],
        position: Position::End,
    })
    .await
    .unwrap();

    c2.learn(LearnParams {
        topic_key: "beta".into(),
        title: Some("Beta".into()),
        summary: Some("second".into()),
        content: "content beta".into(),
        voice: None,
        tags: vec!["b".into()],
        position: Position::End,
    })
    .await
    .unwrap();

    let edge = c1
        .connect_topics(ConnectParams {
            from_key: "alpha".into(),
            to_key: "beta".into(),
            edge_type: EdgeKind::DependsOn,
            note: "alpha needs beta".into(),
            severity: None,
        })
        .await
        .unwrap();
    assert_eq!(edge.action, "created");

    // Both clients see the same state.
    let stats1 = c1.stats().await.unwrap();
    let stats2 = c2.stats().await.unwrap();
    assert_eq!(stats1.topics.total, 2);
    assert_eq!(stats2.topics.total, 2);
    assert_eq!(stats1.edges.total, 1);
    assert_eq!(stats2.edges.total, 1);

    let view = c1.graph_view().await.unwrap();
    assert_eq!(view.topics.len(), 2);
    assert_eq!(view.edges.len(), 1);
}

#[tokio::test]
async fn many_concurrent_writes_serialize_safely() {
    let db = temp_db();
    let _server = spawn_server(&db).await;

    let c1 = std::sync::Arc::new(CairnClient::connect(&db).await.unwrap());
    c1.init_defaults(None).await.unwrap();

    // Fan out 30 concurrent learns from one client process.
    let mut handles = Vec::new();
    for i in 0..30 {
        let c = c1.clone();
        handles.push(tokio::spawn(async move {
            c.learn(LearnParams {
                topic_key: format!("t{i}"),
                title: Some(format!("Topic {i}")),
                summary: Some(format!("summary {i}")),
                content: format!("content {i}"),
                voice: None,
                tags: vec![],
                position: Position::End,
            })
            .await
        }));
    }
    for h in handles {
        h.await.unwrap().unwrap();
    }

    let stats = c1.stats().await.unwrap();
    assert_eq!(stats.topics.total, 30);
}

#[tokio::test]
async fn second_server_refuses_cleanly() {
    let db = temp_db();
    let _server = spawn_server(&db).await;

    // A second cairn-server should detect the flock and exit cleanly with code 0.
    let out = Command::new(server_bin())
        .arg("--db")
        .arg(&db)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run second server");
    assert!(
        out.status.success(),
        "second server should exit cleanly, got: {:?} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test]
async fn rename_through_daemon() {
    let db = temp_db();
    let _server = spawn_server(&db).await;
    let c = CairnClient::connect(&db).await.unwrap();
    c.init_defaults(None).await.unwrap();

    c.learn(LearnParams {
        topic_key: "old-name".into(),
        title: Some("Old".into()),
        summary: None,
        content: "content".into(),
        voice: None,
        tags: vec![],
        position: Position::End,
    })
    .await
    .unwrap();

    let result = c
        .rename(RenameParams {
            old_key: "old-name".into(),
            new_key: "new-name".into(),
        })
        .await
        .unwrap();
    assert_eq!(result.new_key, "new-name");

    let view = c.graph_view().await.unwrap();
    assert!(view.topics.iter().any(|t| t.key == "new-name"));
    assert!(!view.topics.iter().any(|t| t.key == "old-name"));
}

#[tokio::test]
async fn typed_error_round_trip() {
    let db = temp_db();
    let _server = spawn_server(&db).await;
    let c = CairnClient::connect(&db).await.unwrap();

    let err = c
        .connect_topics(ConnectParams {
            from_key: "missing-1".into(),
            to_key: "missing-2".into(),
            edge_type: EdgeKind::DependsOn,
            note: "n".into(),
            severity: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, CairnError::TopicNotFound(_)));
}

// ── Editor session tests ─────────────────────────────────────────

#[tokio::test]
async fn editor_session_blocks_other_clients_mutations_but_not_reads() {
    let db = temp_db();
    let _server = spawn_server(&db).await;

    let editor = CairnClient::connect(&db).await.unwrap();
    let agent = CairnClient::connect(&db).await.unwrap();

    editor.init_defaults(Some("voice")).await.unwrap();

    // Pre-seed a topic so the agent has something to read.
    editor
        .learn(LearnParams {
            topic_key: "alpha".into(),
            title: Some("Alpha".into()),
            summary: Some("first".into()),
            content: "before lock".into(),
            voice: None,
            tags: vec![],
            position: Position::End,
        })
        .await
        .unwrap();

    // Editor acquires the lock with an explicit reason.
    editor
        .begin_editor_session(Some("manual triage"))
        .await
        .unwrap();

    // Status reflects the holder.
    let info = agent
        .editor_session_status()
        .await
        .unwrap()
        .expect("editor session active");
    assert_eq!(info.reason.as_deref(), Some("manual triage"));

    // Agent reads still work.
    let stats = agent.stats().await.unwrap();
    assert_eq!(stats.topics.total, 1);
    let _ = agent
        .prime(PrimeParams {
            task: "anything".into(),
            max_tokens: None,
        })
        .await
        .unwrap();
    let _ = agent.graph_status().await.unwrap();

    // Agent mutations are rejected with the typed EditorBusy variant
    // carrying the structured holder info.
    let err = agent
        .learn(LearnParams {
            topic_key: "beta".into(),
            title: Some("Beta".into()),
            summary: Some("second".into()),
            content: "should be rejected".into(),
            voice: None,
            tags: vec![],
            position: Position::End,
        })
        .await
        .unwrap_err();
    match err {
        CairnError::EditorBusy { reason, .. } => {
            assert_eq!(reason.as_deref(), Some("manual triage"));
        }
        other => panic!("expected EditorBusy, got {other:?}"),
    }

    // The editor itself can still mutate while holding the lock.
    editor
        .learn(LearnParams {
            topic_key: "gamma".into(),
            title: Some("Gamma".into()),
            summary: Some("written by editor".into()),
            content: "this should land".into(),
            voice: None,
            tags: vec![],
            position: Position::End,
        })
        .await
        .unwrap();

    // Release. Agent mutations now succeed.
    editor.end_editor_session().await.unwrap();
    assert!(agent.editor_session_status().await.unwrap().is_none());

    agent
        .learn(LearnParams {
            topic_key: "delta".into(),
            title: Some("Delta".into()),
            summary: Some("after release".into()),
            content: "now allowed".into(),
            voice: None,
            tags: vec![],
            position: Position::End,
        })
        .await
        .unwrap();

    let final_stats = agent.stats().await.unwrap();
    // alpha (pre-lock), gamma (editor during lock), delta (agent after release).
    assert_eq!(final_stats.topics.total, 3);
}

#[tokio::test]
async fn editor_lock_release_on_connection_drop() {
    // The keystone-tier guarantee: if the holder's connection dies (clean
    // exit *or* crash), the daemon releases the lock automatically. There
    // is no other recovery path — we rely entirely on the kernel noticing
    // the dropped socket.
    let db = temp_db();
    let _server = spawn_server(&db).await;

    {
        let editor = CairnClient::connect(&db).await.unwrap();
        editor.init_defaults(Some("voice")).await.unwrap();
        editor
            .begin_editor_session(Some("will be dropped"))
            .await
            .unwrap();

        // Confirm the lock is held from the daemon's perspective.
        let observer = CairnClient::connect(&db).await.unwrap();
        let info = observer
            .editor_session_status()
            .await
            .unwrap()
            .expect("lock held");
        assert_eq!(info.reason.as_deref(), Some("will be dropped"));

        // editor goes out of scope here → its UnixStream drops →
        // cairn-server's read_line returns 0 → handle_connection's
        // cleanup releases the lock.
    }

    // Give the daemon a moment to process the disconnect.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let observer = CairnClient::connect(&db).await.unwrap();
    let status = observer.editor_session_status().await.unwrap();
    assert!(
        status.is_none(),
        "lock should have been released on connection drop, but got {status:?}"
    );

    // And mutations from a fresh client work again.
    observer
        .learn(LearnParams {
            topic_key: "post-drop".into(),
            title: Some("Post Drop".into()),
            summary: Some("written after lock release".into()),
            content: "ok".into(),
            voice: None,
            tags: vec![],
            position: Position::End,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn editor_lock_is_idempotent_for_holder_and_no_op_release_for_others() {
    let db = temp_db();
    let _server = spawn_server(&db).await;

    let editor = CairnClient::connect(&db).await.unwrap();
    let other = CairnClient::connect(&db).await.unwrap();

    editor.init_defaults(Some("voice")).await.unwrap();

    // First Begin succeeds.
    editor
        .begin_editor_session(Some("first reason"))
        .await
        .unwrap();

    // Re-Begin from the same connection updates the reason instead of erroring.
    editor
        .begin_editor_session(Some("updated reason"))
        .await
        .unwrap();
    let info = other
        .editor_session_status()
        .await
        .unwrap()
        .expect("still held");
    assert_eq!(info.reason.as_deref(), Some("updated reason"));

    // Re-Begin with no reason clears the reason but keeps the lock.
    editor.begin_editor_session(None).await.unwrap();
    let info = other
        .editor_session_status()
        .await
        .unwrap()
        .expect("still held");
    assert_eq!(info.reason, None);

    // Begin from a *different* connection is rejected with EditorBusy.
    let err = other
        .begin_editor_session(Some("steal attempt"))
        .await
        .unwrap_err();
    assert!(matches!(err, CairnError::EditorBusy { .. }));

    // End from a non-holder is a silent no-op (lock stays held).
    other.end_editor_session().await.unwrap();
    assert!(other.editor_session_status().await.unwrap().is_some());

    // End from the holder releases.
    editor.end_editor_session().await.unwrap();
    assert!(other.editor_session_status().await.unwrap().is_none());

    // End-when-not-held is also a silent no-op.
    editor.end_editor_session().await.unwrap();
}

#[tokio::test]
async fn client_reconnects_after_daemon_restart() {
    // Simulates the install.sh upgrade flow: a long-lived client (e.g. a
    // Claude Code MCP session) holds a cached connection, the daemon is
    // SIGTERMed and replaced, and the client's next call should
    // transparently reconnect to the new daemon instead of bubbling a
    // BrokenPipe error to the user.
    let db = temp_db();
    let mut server_a = spawn_server(&db).await;

    let c = CairnClient::connect(&db).await.unwrap();
    c.init_defaults(Some("voice")).await.unwrap();
    let stats_before = c.stats().await.unwrap();
    assert_eq!(stats_before.topics.total, 0);

    // Kill daemon A but keep the DB directory around. Wait long enough
    // for the kernel to reap the process and release the flock.
    server_a.kill_now();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Spawn daemon B on the same DB path. The client still holds a
    // cached UnixStream pointing at A's now-dead listener.
    let _server_b = spawn_server(&db).await;

    // The first call after restart triggers the recovery path:
    // try_call hits BrokenPipe → CallError::is_connection_dead → reconnect()
    // → retry on the fresh socket → success.
    let stats_after = c.stats().await.unwrap();
    assert_eq!(stats_after.topics.total, 0);

    // And subsequent calls keep working through the reconnected stream.
    c.learn(LearnParams {
        topic_key: "post-restart".into(),
        title: Some("Post Restart".into()),
        summary: Some("written through the reconnected socket".into()),
        content: "if you can read me, the reconnect worked".into(),
        voice: None,
        tags: vec![],
        position: Position::End,
    })
    .await
    .unwrap();

    let stats_final = c.stats().await.unwrap();
    assert_eq!(stats_final.topics.total, 1);
}
