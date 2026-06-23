//! Rebasing a stack onto a new trunk, in-process.
//!
//! We reparent the stack's bottom commit onto the new trunk and let
//! `transform_descendants` carry the rest of the stack along. jj records
//! conflicts inside the rewritten commits rather than stopping, so the caller
//! must inspect the result for conflicts afterward. Bookmarks pointing at
//! rewritten commits are moved automatically.

use anyhow::{Context, Result, anyhow};
use jj_lib::backend::CommitId;
use jj_lib::repo::Repo;
use jj_lib::rewrite::CommitRewriter;

use crate::jj::repo::LoadedRepo;

/// Rebase the stack whose bottom is `bottom` so that `bottom`'s parent becomes
/// `new_parent` (trunk). No-op if it is already the sole parent. Returns true if
/// a rebase was performed.
pub async fn rebase_stack_onto(
    loaded: &LoadedRepo,
    bottom: &CommitId,
    new_parent: &CommitId,
) -> Result<bool> {
    let bottom_commit = loaded
        .repo
        .store()
        .get_commit(bottom)
        .context("failed to load stack bottom commit")?;
    if bottom_commit.parent_ids() == [new_parent.clone()] {
        return Ok(false);
    }

    let mut tx = loaded.repo.start_transaction();
    // Reparent the bottom commit onto the new trunk, then let jj rebase the rest
    // of the stack (descendants) onto the rewritten bottom.
    {
        let rewriter =
            CommitRewriter::new(tx.repo_mut(), bottom_commit, vec![new_parent.clone()]);
        rewriter
            .rebase()
            .await
            .map_err(|e| anyhow!(e))
            .context("failed to reparent stack bottom")?
            .write()
            .await
            .map_err(|e| anyhow!(e))
            .context("failed to write reparented bottom")?;
    }
    tx.repo_mut()
        .rebase_descendants()
        .await
        .map_err(|e| anyhow!(e))
        .context("failed to rebase stack descendants")?;

    tx.commit("stacker: rebase stack onto trunk")
        .await
        .map_err(|e| anyhow!(e))
        .context("failed to commit rebase")?;
    Ok(true)
}

/// Abandon `target`, rebasing its descendants onto `target`'s parent(s).
///
/// Used to drop the bottom of a stack whose PR was closed without merging: the
/// change's diff is discarded and the changes above it become children of its
/// parent (trunk, for a stack bottom). jj records any resulting conflicts inside
/// the rewritten commits rather than stopping, so the caller must inspect for
/// conflicts afterward. A bookmark at `target` is moved to the parent or deleted
/// per jj's default rewrite-refs policy. The abandon is recoverable via `jj op`.
pub async fn abandon_change(loaded: &LoadedRepo, target: &CommitId) -> Result<()> {
    let commit = loaded
        .repo
        .store()
        .get_commit(target)
        .context("failed to load change to abandon")?;

    let mut tx = loaded.repo.start_transaction();
    tx.repo_mut().record_abandoned_commit(&commit);
    tx.repo_mut()
        .rebase_descendants()
        .await
        .map_err(|e| anyhow!(e))
        .context("failed to rebase descendants after abandon")?;

    tx.commit("stacker: abandon dropped change")
        .await
        .map_err(|e| anyhow!(e))
        .context("failed to commit abandon")?;
    Ok(())
}
