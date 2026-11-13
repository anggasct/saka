use std::path::PathBuf;

use saka_ports::{EnvRef, PortError, PortResult, REVERT_PATHS_MAX, RevertRequest, Reverted};
use serde_json::json;

use super::{GIT_NOT_AN_ANCESTOR, GitRepos, is_sha, sha_arg};
use crate::git::{Extra, failed};

const MENTION_SCAN: &str = "2";

enum Shape {
    MergeCommit,
    Squash,
    Range(u32),
}

impl GitRepos {
    pub(super) async fn revert_commit(
        &self,
        env: &EnvRef,
        request: &RevertRequest,
    ) -> PortResult<Reverted> {
        sha_arg(&request.base_sha, "base_sha")?;
        sha_arg(&request.merge_commit, "merge_commit")?;
        if let Some(head) = &request.head_sha {
            sha_arg(head, "head_sha")?;
        }
        let wt = PathBuf::from(&env.worktree_path);
        if !wt.join(".git").exists() {
            return Err(PortError::permanent(
                "worktree_missing",
                json!({ "environment_id": env.environment_id }),
            ));
        }
        let lock = self.lock(env.repo.repo_id);
        let _guard = lock.lock().await;
        let bare = self.mirror_path(env.repo.repo_id);
        let merged = request.merge_commit.as_str();
        if !self.has_commit(&bare, merged).await? {
            return Ok(Reverted::NotInBase);
        }
        self.git_ok(
            &wt,
            &["reset", "--hard", "--quiet", &request.base_sha],
            "reset",
        )
        .await?;
        self.git_ok(&wt, &["clean", "-fdq"], "clean").await?;
        let ancestry = self
            .cfg
            .git
            .local(&wt, &["merge-base", "--is-ancestor", merged, "HEAD"])
            .await?;
        match ancestry.code {
            Some(0) => {}
            Some(GIT_NOT_AN_ANCESTOR) => return Ok(Reverted::NotInBase),
            _ => return Err(failed("merge_base", &ancestry)),
        }
        let Some(shape) = self.merged_shape(&wt, &bare, request).await? else {
            return Ok(Reverted::CommitsUnknown);
        };
        let range;
        let mut args = vec!["-c", "core.hooksPath=/dev/null", "revert", "--no-commit"];
        match shape {
            Shape::MergeCommit => args.extend(["-m", "1", merged]),
            Shape::Squash => args.push(merged),
            Shape::Range(n) => {
                range = format!("{merged}~{n}..{merged}");
                args.push(&range);
            }
        }
        let identity = [
            ("GIT_AUTHOR_NAME", request.author.name.as_str()),
            ("GIT_AUTHOR_EMAIL", request.author.email.as_str()),
            ("GIT_COMMITTER_NAME", request.committer.name.as_str()),
            ("GIT_COMMITTER_EMAIL", request.committer.email.as_str()),
        ];
        let git = &self.cfg.git;
        let applied = git
            .local_with(
                &wt,
                &args,
                Extra {
                    env: &identity,
                    input: None,
                },
            )
            .await?;
        if !applied.ok() {
            let unmerged = git
                .local(&wt, &["diff", "--name-only", "--diff-filter=U", "-z"])
                .await?;
            let mut paths: Vec<String> = unmerged
                .stdout
                .split(|b| *b == 0)
                .filter(|p| !p.is_empty())
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect();
            if paths.is_empty() {
                return Err(failed("revert", &applied));
            }
            self.discard_revert(&wt).await?;
            let count = paths.len();
            paths.truncate(REVERT_PATHS_MAX);
            return Ok(Reverted::Conflict { paths, count });
        }
        let staged = git.local(&wt, &["diff", "--cached", "--quiet"]).await?;
        match staged.code {
            Some(0) => {
                self.discard_revert(&wt).await?;
                return Ok(Reverted::Empty);
            }
            Some(1) => {}
            _ => return Err(failed("diff", &staged)),
        }
        let committed = git
            .local_with(
                &wt,
                &[
                    "-c",
                    "core.hooksPath=/dev/null",
                    "commit",
                    "--quiet",
                    "--no-verify",
                    "-F",
                    "-",
                ],
                Extra {
                    env: &identity,
                    input: Some(request.message.as_bytes()),
                },
            )
            .await?;
        if !committed.ok() {
            return Err(failed("commit", &committed));
        }
        let head = self
            .git_ok(&wt, &["rev-parse", "HEAD"], "rev_parse")
            .await?;
        Ok(Reverted::Committed {
            head_sha: head.text(),
        })
    }

    async fn discard_revert(&self, wt: &std::path::Path) -> PortResult<()> {
        let git = &self.cfg.git;
        git.local(wt, &["revert", "--abort"]).await?;
        self.git_ok(wt, &["reset", "--hard", "--quiet", "HEAD"], "reset")
            .await?;
        self.git_ok(wt, &["clean", "-fdq"], "clean").await?;
        Ok(())
    }

    async fn merged_shape(
        &self,
        wt: &std::path::Path,
        bare: &std::path::Path,
        request: &RevertRequest,
    ) -> PortResult<Option<Shape>> {
        let merged = request.merge_commit.as_str();
        let parents = self
            .git_ok(
                bare,
                &["rev-list", "--parents", "-n", "1", merged],
                "rev_list",
            )
            .await?
            .text();
        if parents.split_whitespace().count() > 2 {
            return Ok(Some(Shape::MergeCommit));
        }
        let head = match &request.head_sha {
            Some(head) if self.has_commit(bare, head).await? => head.as_str(),
            _ => return self.sole_mention(wt, request).await,
        };
        let fork = self
            .cfg
            .git
            .local(bare, &["merge-base", head, merged])
            .await?;
        if !fork.ok() {
            return self.sole_mention(wt, request).await;
        }
        let fork = fork.text();
        let spanned = format!("{fork}..{head}");
        let counted = self
            .git_ok(bare, &["rev-list", "--count", &spanned], "rev_list")
            .await?
            .text();
        let n: u32 = counted
            .parse()
            .map_err(|_| PortError::retryable("git_output_invalid", json!({ "op": "rev_list" })))?;
        if n <= 1 {
            return Ok(Some(Shape::Squash));
        }
        let first = format!("{merged}~{n}");
        let exists = self
            .cfg
            .git
            .local(bare, &["rev-parse", "--verify", "--quiet", &first])
            .await?;
        if !exists.ok() {
            return Ok(Some(Shape::Squash));
        }
        let tail = format!("{first}..{merged}");
        let (landed, authored) = (
            self.patches_of(bare, &tail).await?,
            self.patches_of(bare, &spanned).await?,
        );
        Ok(Some(if landed == authored {
            Shape::Range(n)
        } else {
            Shape::Squash
        }))
    }

    async fn patches_of(&self, bare: &std::path::Path, range: &str) -> PortResult<Vec<String>> {
        let ids = self
            .patch_ids(
                bare,
                &[
                    "log",
                    "-p",
                    "--reverse",
                    "--no-merges",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--format=commit %H",
                    range,
                ],
            )
            .await?;
        Ok(ids.into_iter().map(|(id, _)| id).collect())
    }

    async fn sole_mention(
        &self,
        wt: &std::path::Path,
        request: &RevertRequest,
    ) -> PortResult<Option<Shape>> {
        let pattern = format!("(#{})", request.change_number);
        let found = self
            .git_ok(
                wt,
                &[
                    "log",
                    "--format=%H",
                    "--fixed-strings",
                    "-n",
                    MENTION_SCAN,
                    &format!("--grep={pattern}"),
                    "HEAD",
                ],
                "log",
            )
            .await?
            .text();
        let hits: Vec<&str> = found.lines().filter(|l| is_sha(l)).collect();
        Ok((hits == [request.merge_commit.as_str()]).then_some(Shape::Squash))
    }
}
