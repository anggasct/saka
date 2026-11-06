use saka_core::decision::Question;
use saka_core::ids::{DecisionRequestId, SessionId};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

pub const MAX_LINE_BYTES: u64 = 64 * 1024;
pub const UNAUTHORIZED: &str = "unauthorized";
pub const MALFORMED: &str = "malformed";

#[derive(Serialize, Deserialize)]
pub struct Request {
    pub session: SessionId,
    pub token: String,
    #[serde(flatten)]
    pub op: Op,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Hello,
    Ask { question: Question },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    Hello,
    Asked { decision_id: DecisionRequestId },
    Refused { code: String },
}

pub async fn read_line<R>(reader: R) -> std::io::Result<Option<String>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut line = Vec::new();
    let mut reader = BufReader::new(reader.take(MAX_LINE_BYTES));
    if reader.read_until(b'\n', &mut line).await? == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "line is incomplete or too long",
        ));
    }
    String::from_utf8(line)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub async fn write_line<W, T>(writer: &mut W, value: &T) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await
}
