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
    child: std::process::Child,
    db_dir: PathBuf,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        // SIGTERM the child, give it a moment, then SIGKILL if needed.
        let _ = self.child.kill();
        let _ = self.child.wait();
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
        child,
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
            from: "alpha".into(),
            to: "beta".into(),
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
            from: "missing-1".into(),
            to: "missing-2".into(),
            edge_type: EdgeKind::DependsOn,
            note: "n".into(),
            severity: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, CairnError::TopicNotFound(_)));
}
