use std::path::PathBuf;
use std::time::Duration;

use saka_core::decision::Question;
use saka_core::ids::{DecisionRequestId, SessionId};
use saka_ports::{AskRefusal, RegressionRefusal, Requested};
use tokio::net::UnixStream;

use crate::wire::{self, Op, Reply, Request, UNAUTHORIZED};

const CALL_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("the engine socket is not reachable: {0}")]
    Unreachable(std::io::Error),
    #[error("the engine refused the session token")]
    Unauthorized,
    #[error("the engine answered something unexpected")]
    Protocol,
    #[error("{0}")]
    Refused(AskRefusal),
    #[error("{0}")]
    RegressionRefused(RegressionRefusal),
}

pub struct Client {
    socket: PathBuf,
    session: SessionId,
    token: String,
}

impl Client {
    pub fn new(socket: PathBuf, session: SessionId, token: String) -> Client {
        Client {
            socket,
            session,
            token,
        }
    }

    pub async fn hello(&self) -> Result<(), ClientError> {
        match self.call(Op::Hello).await? {
            Reply::Hello => Ok(()),
            _ => Err(ClientError::Protocol),
        }
    }

    pub async fn ask(&self, question: Question) -> Result<DecisionRequestId, ClientError> {
        match self.call(Op::Ask { question }).await? {
            Reply::Asked { decision_id } => Ok(decision_id),
            Reply::Refused { code } => {
                Err(AskRefusal::from_code(&code)
                    .map_or(ClientError::Protocol, ClientError::Refused))
            }
            _ => Err(ClientError::Protocol),
        }
    }

    pub async fn regression_check(&self) -> Result<Requested, ClientError> {
        match self.call(Op::RegressionCheck).await? {
            Reply::RegressionRecorded {
                attempt,
                commit_sha,
            } => Ok(Requested::Recorded {
                attempt,
                commit_sha,
            }),
            Reply::RegressionDirty => Ok(Requested::DirtyWorktree),
            Reply::Refused { code } => Err(RegressionRefusal::from_code(&code)
                .map_or(ClientError::Protocol, ClientError::RegressionRefused)),
            _ => Err(ClientError::Protocol),
        }
    }

    async fn call(&self, op: Op) -> Result<Reply, ClientError> {
        let request = Request {
            session: self.session,
            token: self.token.clone(),
            op,
        };
        let reply = tokio::time::timeout(CALL_TIMEOUT, self.exchange(&request))
            .await
            .map_err(|_| ClientError::Unreachable(std::io::ErrorKind::TimedOut.into()))?
            .map_err(ClientError::Unreachable)?;
        match reply {
            Reply::Refused { code } if code == UNAUTHORIZED => Err(ClientError::Unauthorized),
            other => Ok(other),
        }
    }

    async fn exchange(&self, request: &Request) -> std::io::Result<Reply> {
        let mut stream = UnixStream::connect(&self.socket).await?;
        let (read, mut write) = stream.split();
        wire::write_line(&mut write, request).await?;
        let line = wire::read_line(read)
            .await?
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?;
        serde_json::from_str(&line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}
