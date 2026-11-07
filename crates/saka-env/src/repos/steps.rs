use std::path::Path;

use async_trait::async_trait;
use saka_ports::{
    CommitPush, GitRemote, PortError, PortResult, PushCommit, PushOutcome, PushRequest,
};
use serde_json::json;

use super::{GitRepos, is_sha};
use crate::git::{GitOutput, auth_refused, failed};

pub(super) enum Step<T> {
    Done(T),
    Failed(GitOutput),
}

#[async_trait]
pub(super) trait AsyncStep<T>: Send {
    async fn run(
        &mut self,
        repos: &GitRepos,
        bare: &Path,
        remote: &GitRemote,
    ) -> PortResult<Step<T>>;
}

pub(super) struct Fetch;

#[async_trait]
impl AsyncStep<()> for Fetch {
    async fn run(
        &mut self,
        repos: &GitRepos,
        bare: &Path,
        remote: &GitRemote,
    ) -> PortResult<Step<()>> {
        let out = repos
            .cfg
            .git
            .remote(bare, &["fetch", "--prune", "--quiet", "origin"], remote)
            .await?;
        Ok(if out.ok() {
            Step::Done(())
        } else {
            Step::Failed(out)
        })
    }
}

pub(super) struct FetchRef<'a> {
    pub(super) refspec: &'a str,
}

#[async_trait]
impl AsyncStep<()> for FetchRef<'_> {
    async fn run(
        &mut self,
        repos: &GitRepos,
        bare: &Path,
        remote: &GitRemote,
    ) -> PortResult<Step<()>> {
        let out = repos
            .cfg
            .git
            .remote(bare, &["fetch", "--quiet", "origin", self.refspec], remote)
            .await?;
        Ok(if out.ok() {
            Step::Done(())
        } else {
            Step::Failed(out)
        })
    }
}

pub(super) struct Push<'a> {
    pub(super) request: &'a PushRequest,
}

pub(super) struct PushOne<'a> {
    pub(super) request: &'a PushCommit,
}

#[async_trait]
impl AsyncStep<CommitPush> for PushOne<'_> {
    async fn run(
        &mut self,
        repos: &GitRepos,
        bare: &Path,
        remote: &GitRemote,
    ) -> PortResult<Step<CommitPush>> {
        let req = self.request;
        let refname = format!("refs/heads/{}", req.branch);
        let git = &repos.cfg.git;
        // The lease turns the update into compare-and-swap on the remote: it lands only while the
        // branch is still at `expected`, and `commit` is a child of it, so it is a fast-forward.
        let lease = format!("--force-with-lease={refname}:{}", req.expected);
        let spec = format!("{}:{refname}", req.commit);
        let out = git
            .remote(
                bare,
                &["push", "--porcelain", "--quiet", &lease, "origin", &spec],
                remote,
            )
            .await?;
        // Porcelain marks a refused ref with a leading `!` status line. A failure without one never
        // reached a decision on the remote, for example when the network is down.
        let refused = out
            .stdout
            .split(|b| *b == b'\n')
            .any(|line| line.starts_with(b"!\t"));
        if !out.ok() && !refused {
            return Ok(Step::Failed(out));
        }
        let pushed = if out.ok() {
            true
        } else {
            let ls = git
                .remote(bare, &["ls-remote", "origin", &refname], remote)
                .await?;
            if !ls.ok() {
                return Ok(Step::Failed(ls));
            }
            match remote_sha(&ls, &refname) {
                Some(sha) if sha == req.commit => true,
                Some(sha) if sha == req.expected => return Ok(Step::Done(CommitPush::Rejected)),
                remote_sha => return Ok(Step::Done(CommitPush::Moved { remote_sha })),
            }
        };
        if pushed {
            let tracking = format!("refs/remotes/origin/{}", req.branch);
            // A fetch may already have moved the tracking ref; the next fetch corrects it anyway.
            git.local(bare, &["update-ref", &tracking, &req.commit, &req.expected])
                .await?;
        }
        Ok(Step::Done(CommitPush::Pushed))
    }
}

pub(super) fn remote_sha(out: &GitOutput, refname: &str) -> Option<String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| {
            let (sha, name) = line.split_once('\t')?;
            (name == refname && is_sha(sha)).then(|| sha.to_string())
        })
}

#[async_trait]
impl AsyncStep<PushOutcome> for Push<'_> {
    async fn run(
        &mut self,
        repos: &GitRepos,
        bare: &Path,
        remote: &GitRemote,
    ) -> PortResult<Step<PushOutcome>> {
        let req = self.request;
        let refname = format!("refs/heads/{}", req.branch);
        let git = &repos.cfg.git;
        let ls = git
            .remote(bare, &["ls-remote", "origin", &refname], remote)
            .await?;
        if !ls.ok() {
            return Ok(Step::Failed(ls));
        }
        let current = remote_sha(&ls, &refname);
        match &current {
            Some(sha) if *sha == req.local_sha => {
                return Ok(Step::Done(PushOutcome::Pushed { sha: sha.clone() }));
            }
            Some(sha) if Some(sha) != req.expected_remote_sha.as_ref() => {
                return Ok(Step::Done(PushOutcome::Rejected {
                    remote_sha: sha.clone(),
                }));
            }
            _ => {}
        }
        // An empty lease means "the branch must not exist yet".
        let lease = format!(
            "--force-with-lease={refname}:{}",
            current.clone().unwrap_or_default()
        );
        let local = git
            .local(bare, &["rev-parse", "--verify", "--quiet", &refname])
            .await?;
        if !local.ok() || local.text() != req.local_sha {
            return Err(PortError::permanent(
                "branch_moved_locally",
                json!({ "branch": req.branch, "expected": req.local_sha, "found": local.text() }),
            ));
        }
        let spec = format!("{0}:{0}", req.branch);
        let out = git
            .remote(bare, &["push", "--quiet", &lease, "origin", &spec], remote)
            .await?;
        if out.ok() {
            return Ok(Step::Done(PushOutcome::Pushed {
                sha: req.local_sha.clone(),
            }));
        }
        if auth_refused(&out.stderr) {
            return Ok(Step::Failed(out));
        }
        let again = git
            .remote(bare, &["ls-remote", "origin", &refname], remote)
            .await?;
        match remote_sha(&again, &refname) {
            Some(sha) if sha == req.local_sha => Ok(Step::Done(PushOutcome::Pushed { sha })),
            Some(sha) if Some(&sha) != current.as_ref() => {
                Ok(Step::Done(PushOutcome::Rejected { remote_sha: sha }))
            }
            _ => Err(failed("push", &out)),
        }
    }
}
