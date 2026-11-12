use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use saka_core::ids::SessionId;
use saka_core::secret::SecretText;
use saka_ports::{AskRefusal, Decisions, Regressions, Requested};
use subtle::ConstantTimeEq;
use tokio::net::{UnixListener, UnixStream};

use crate::wire::{self, MALFORMED, Op, Reply, Request, UNAUTHORIZED};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

pub trait Tokens: Send + Sync {
    fn token(&self, session: SessionId) -> Option<SecretText>;
}

pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub async fn serve(
    listener: UnixListener,
    tokens: Arc<dyn Tokens>,
    decisions: Arc<dyn Decisions>,
    regressions: Arc<dyn Regressions>,
) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let tokens = Arc::clone(&tokens);
        let decisions = Arc::clone(&decisions);
        let regressions = Arc::clone(&regressions);
        tokio::spawn(async move {
            if let Err(e) = tokio::time::timeout(
                REQUEST_TIMEOUT,
                handle(
                    stream,
                    tokens.as_ref(),
                    decisions.as_ref(),
                    regressions.as_ref(),
                ),
            )
            .await
            .unwrap_or_else(|_| Err(std::io::Error::from(std::io::ErrorKind::TimedOut)))
            {
                tracing::debug!(error = %e, "mcp socket request failed");
            }
        });
    }
}

fn authorized(tokens: &dyn Tokens, request: &Request) -> bool {
    tokens.token(request.session).is_some_and(|expected| {
        expected
            .expose()
            .as_bytes()
            .ct_eq(request.token.as_bytes())
            .into()
    })
}

async fn handle(
    mut stream: UnixStream,
    tokens: &dyn Tokens,
    decisions: &dyn Decisions,
    regressions: &dyn Regressions,
) -> std::io::Result<()> {
    let (read, mut write) = stream.split();
    let reply = match wire::read_line(read).await? {
        None => return Ok(()),
        Some(line) => match serde_json::from_str::<Request>(&line) {
            Err(_) => refused(MALFORMED),
            Ok(request) if !authorized(tokens, &request) => refused(UNAUTHORIZED),
            Ok(Request { op: Op::Hello, .. }) => Reply::Hello,
            Ok(Request {
                session,
                op: Op::Ask { question },
                ..
            }) => {
                if question.validate().is_err() {
                    refused(AskRefusal::InvalidInput.code())
                } else {
                    match decisions.ask(session, question).await {
                        Ok(asked) => Reply::Asked {
                            decision_id: asked.decision_id,
                        },
                        Err(refusal) => refused(refusal.code()),
                    }
                }
            }
            Ok(Request {
                session,
                op: Op::RegressionCheck,
                ..
            }) => match regressions.request(session).await {
                Ok(Requested::Recorded {
                    attempt,
                    commit_sha,
                }) => Reply::RegressionRecorded {
                    attempt,
                    commit_sha,
                },
                Ok(Requested::DirtyWorktree) => Reply::RegressionDirty,
                Err(refusal) => refused(refusal.code()),
            },
        },
    };
    wire::write_line(&mut write, &reply).await
}

fn refused(code: &str) -> Reply {
    Reply::Refused {
        code: code.to_string(),
    }
}
