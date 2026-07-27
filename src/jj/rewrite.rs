//! Rebasing a stack onto a new trunk, in-process.
//!
//! We reparent the stack's bottom commit onto the new trunk and let
//! `transform_descendants` carry the rest of the stack along. jj records
//! conflicts inside the rewritten commits rather than stopping, so the caller
//! must inspect the result for conflicts afterward. Bookmarks pointing at
//! rewritten commits are moved automatically.

use anyhow::{Context, Result, anyhow};
use jj_lib::backend::CommitId;
use jj_lib::op_store::RefTarget;
use jj_lib::ref_name::RefName;
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

/// Abandon every commit in `targets` (a whole stack) in one transaction,
/// rebasing any outside descendants onto the nearest surviving ancestor, and
/// delete the given local bookmarks. The bookmarks are deleted explicitly —
/// before descendants are rebased — so the rewrite-refs policy doesn't leave
/// them piled up on trunk. If a target is the working-copy commit, jj-lib
/// recreates a fresh empty `@` on its parent. Recoverable via `jj undo`.
pub async fn abandon_stack(
    loaded: &LoadedRepo,
    targets: &[CommitId],
    bookmarks: &[String],
) -> Result<()> {
    let mut tx = loaded.repo.start_transaction();
    for target in targets {
        let commit = loaded
            .repo
            .store()
            .get_commit(target)
            .context("failed to load change to abandon")?;
        tx.repo_mut().record_abandoned_commit(&commit);
    }
    for name in bookmarks {
        tx.repo_mut()
            .set_local_bookmark_target(RefName::new(name), RefTarget::absent());
    }
    tx.repo_mut()
        .rebase_descendants()
        .await
        .map_err(|e| anyhow!(e))
        .context("failed to rebase descendants after abandoning the stack")?;

    tx.commit("stacker: abandon stack")
        .await
        .map_err(|e| anyhow!(e))
        .context("failed to commit stack abandon")?;
    Ok(())
}
