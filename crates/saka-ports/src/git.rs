use std::fmt;

use async_trait::async_trait;

use crate::PortResult;
use crate::scm::{PushOutcome, PushRequest, RepoRef};

#[derive(Clone, PartialEq, Eq)]
pub struct GitRemote {
    pub url: String,
    pub config: Vec<(String, String)>,
}

impl fmt::Debug for GitRemote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let keys: Vec<&str> = self.config.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("GitRemote")
            .field("url", &self.url)
            .field("config_keys", &keys)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blob {
    Missing,
    TooLarge { bytes: u64 },
    Bytes(Vec<u8>),
}

#[async_trait]
pub trait GitAccess: Send + Sync {
    async fn remote(&self, repo: &RepoRef) -> PortResult<GitRemote>;

    async fn refused(&self, repo: &RepoRef);
}

#[async_trait]
pub trait Repos: Send + Sync {
    async fn fetch(&self, repo: &RepoRef) -> PortResult<()>;

    async fn tip(&self, repo: &RepoRef, branch: &str) -> PortResult<Option<String>>;

    async fn read_file(
        &self,
        repo: &RepoRef,
        commit: &str,
        path: &str,
    ) -> PortResult<Option<Vec<u8>>>;

    async fn read_bounded(
        &self,
        repo: &RepoRef,
        commit: &str,
        path: &str,
        max_bytes: u64,
    ) -> PortResult<Blob>;

    async fn grep_paths(
        &self,
        repo: &RepoRef,
        commit: &str,
        pattern: &str,
        pathspecs: &[String],
    ) -> PortResult<Vec<String>>;

    async fn push(&self, request: &PushRequest) -> PortResult<PushOutcome>;
}
