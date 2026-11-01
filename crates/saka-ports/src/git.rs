//! Saka's own git: one bare mirror per repo, fetched with credentials that only Saka's
//! `git` processes see. The SCM adapter says how to reach a remote ([`GitAccess`]); the
//! environment adapter runs `git` ([`Repos`]).

use std::fmt;

use async_trait::async_trait;

use crate::PortResult;
use crate::scm::{PushOutcome, PushRequest, RepoRef};

/// How Saka's `git` reaches a repo's remote. `config` holds secrets (an auth header) and
/// is given to the `git` child through `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n`/
/// `GIT_CONFIG_VALUE_n`, never through argv or the remote URL.
#[derive(Clone, PartialEq, Eq)]
pub struct GitRemote {
    /// The fetch and push URL, without credentials.
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

#[async_trait]
pub trait GitAccess: Send + Sync {
    async fn remote(&self, repo: &RepoRef) -> PortResult<GitRemote>;

    /// The remote refused the credentials `remote` gave; the next call gets new ones.
    async fn refused(&self, repo: &RepoRef);
}

#[async_trait]
pub trait Repos: Send + Sync {
    /// Fetches every branch of the remote into the mirror (`refs/remotes/origin/*`).
    async fn fetch(&self, repo: &RepoRef) -> PortResult<()>;

    /// The commit `origin/<branch>` points at in the mirror, as last fetched.
    async fn tip(&self, repo: &RepoRef, branch: &str) -> PortResult<Option<String>>;

    /// The bytes of `path` at `commit`, exactly as stored; `None` when there is no such file.
    async fn read_file(
        &self,
        repo: &RepoRef,
        commit: &str,
        path: &str,
    ) -> PortResult<Option<Vec<u8>>>;

    /// Pushes `local_sha` to the remote branch with a lease on the sha Saka knows.
    async fn push(&self, request: &PushRequest) -> PortResult<PushOutcome>;
}
