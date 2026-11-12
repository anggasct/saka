#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test fails loudly"
)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use saka_core::decision::{DecisionOption, Question};
use saka_core::ids::{DecisionRequestId, SessionId};
use saka_core::secret::SecretText;
use saka_mcp::{Client, ClientError, Tokens};
use saka_ports::{AskRefusal, Asked, Decisions, RegressionRefusal, Regressions, Requested};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use uuid::Uuid;

const TOKEN: &str = "socket-test-token";

fn session(n: u128) -> SessionId {
    SessionId(Uuid::from_u128(n))
}

struct OneSession;

impl Tokens for OneSession {
    fn token(&self, id: SessionId) -> Option<SecretText> {
        (id == session(1)).then(|| SecretText::new(TOKEN.to_string()))
    }
}

#[derive(Default)]
struct Recorder {
    asked: Mutex<Vec<SessionId>>,
    checked: Mutex<Vec<SessionId>>,
    reply: Mutex<Vec<Result<Requested, RegressionRefusal>>>,
}

#[async_trait]
impl Regressions for Recorder {
    async fn request(&self, session: SessionId) -> Result<Requested, RegressionRefusal> {
        self.checked.lock().unwrap().push(session);
        self.reply
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Err(RegressionRefusal::ToolUnavailable))
    }
}

#[async_trait]
impl Decisions for Recorder {
    async fn ask(&self, session: SessionId, _question: Question) -> Result<Asked, AskRefusal> {
        self.asked.lock().unwrap().push(session);
        Ok(Asked {
            decision_id: DecisionRequestId(Uuid::from_u128(9)),
        })
    }
}

fn question() -> Question {
    let option = |label: &str| DecisionOption {
        label: label.into(),
        detail: None,
    };
    Question {
        question: "Which one?".into(),
        options: vec![option("A"), option("B")],
        context: None,
    }
}

async fn start(dir: &Path) -> (std::path::PathBuf, Arc<Recorder>) {
    let socket = dir.join("run").join("mcp.sock");
    let listener = saka_mcp::bind(&socket).unwrap();
    let recorder = Arc::new(Recorder::default());
    tokio::spawn(saka_mcp::serve(
        listener,
        Arc::new(OneSession),
        recorder.clone(),
        recorder.clone(),
    ));
    (socket, recorder)
}

#[tokio::test]
async fn the_socket_and_its_folder_are_private() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, _) = start(dir.path()).await;
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&socket), 0o600);
    assert_eq!(mode(socket.parent().unwrap()), 0o700);
}

#[tokio::test]
async fn a_stale_socket_file_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("run")).unwrap();
    std::fs::write(dir.path().join("run").join("mcp.sock"), b"stale").unwrap();
    let (socket, _) = start(dir.path()).await;
    Client::new(socket, session(1), TOKEN.into())
        .hello()
        .await
        .unwrap();
}

#[tokio::test]
async fn only_the_right_token_for_the_right_session_is_served() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, recorder) = start(dir.path()).await;
    let client = |id, token: &str| Client::new(socket.clone(), id, token.into());

    client(session(1), TOKEN).hello().await.unwrap();
    for (id, token) in [(session(1), "wrong"), (session(1), ""), (session(2), TOKEN)] {
        assert!(matches!(
            client(id, token).hello().await,
            Err(ClientError::Unauthorized)
        ));
        assert!(matches!(
            client(id, token).ask(question()).await,
            Err(ClientError::Unauthorized)
        ));
    }
    assert!(recorder.asked.lock().unwrap().is_empty());

    let id = client(session(1), TOKEN).ask(question()).await.unwrap();
    assert_eq!(id, DecisionRequestId(Uuid::from_u128(9)));
    assert_eq!(*recorder.asked.lock().unwrap(), [session(1)]);
}

#[tokio::test]
async fn an_invalid_question_is_refused_by_the_socket_before_the_engine() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, recorder) = start(dir.path()).await;
    let mut bad = question();
    bad.options.pop();
    let refused = Client::new(socket, session(1), TOKEN.into()).ask(bad).await;
    assert!(matches!(
        refused,
        Err(ClientError::Refused(AskRefusal::InvalidInput))
    ));
    assert!(recorder.asked.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_regression_check_reaches_the_engine_only_with_the_right_token_and_returns_its_answer() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, recorder) = start(dir.path()).await;
    let client = |id, token: &str| Client::new(socket.clone(), id, token.into());
    assert!(matches!(
        client(session(1), "wrong").regression_check().await,
        Err(ClientError::Unauthorized)
    ));
    assert!(recorder.checked.lock().unwrap().is_empty());

    let recorded = Requested::Recorded {
        attempt: 2,
        commit_sha: "a".repeat(40),
    };
    recorder.reply.lock().unwrap().extend([
        Ok(Requested::DirtyWorktree),
        Err(RegressionRefusal::LimitReached),
        Ok(recorded.clone()),
    ]);
    let good = client(session(1), TOKEN);
    let ask = || good.regression_check();
    assert_eq!(ask().await.unwrap(), recorded);
    assert!(matches!(
        ask().await,
        Err(ClientError::RegressionRefused(
            RegressionRefusal::LimitReached
        ))
    ));
    assert_eq!(ask().await.unwrap(), Requested::DirtyWorktree);
    assert!(matches!(
        ask().await,
        Err(ClientError::RegressionRefused(
            RegressionRefusal::ToolUnavailable
        ))
    ));
    assert_eq!(recorder.checked.lock().unwrap().len(), 4);
}

async fn raw(socket: &Path, bytes: &[u8]) -> String {
    let mut stream = UnixStream::connect(socket).await.unwrap();
    let _ = stream.write_all(bytes).await;
    let mut reply = String::new();
    let _ = stream.read_to_string(&mut reply).await;
    reply
}

#[tokio::test]
async fn garbage_and_oversized_lines_are_answered_or_dropped_without_reaching_the_engine() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, recorder) = start(dir.path()).await;
    let reply = raw(&socket, b"not json\n").await;
    assert!(reply.contains("malformed"), "{reply}");
    let mut huge = vec![b'x'; 200_000];
    huge.push(b'\n');
    assert_eq!(raw(&socket, &huge).await, "");
    assert!(recorder.asked.lock().unwrap().is_empty());
    Client::new(socket, session(1), TOKEN.into())
        .hello()
        .await
        .unwrap();
}
