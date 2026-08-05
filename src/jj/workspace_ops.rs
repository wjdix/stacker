//! Creating a new change in the stack and moving the working copy to it.

use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use jj_lib::backend::CommitId;
use jj_lib::commit::Commit;
use jj_lib::repo::{ReadonlyRepo, Repo};
use jj_lib::rewrite::CommitRewriter;
use jj_lib::workspace::Workspace;

/// Refresh the working copy to the repo's head operation, so the jj CLI doesn't
/// report a stale working copy after stacker committed transactions.
///
/// stacker mutates the repo via jj-lib transactions (rebase, push, fetch) which
/// advance the operation log but leave the working copy's recorded operation
/// behind. Checking out the current `@` commit at head advances that pointer.
/// When `@`'s tree is unchanged (the common case), the applied diff is empty, so
/// files — including any uncommitted edits — are left untouched.
pub async fn recover(workspace: &mut Workspace, repo: &Arc<ReadonlyRepo>) -> Result<()> {
    if workspace.working_copy().operation_id() == repo.operation().id() {
        return Ok(()); // already fresh
    }
    let wc_name = workspace.workspace_name().to_owned();
    let Some(at_id) = repo.view().get_wc_commit_id(&wc_name).cloned() else {
        return Ok(()); // no working-copy commit (e.g. a bare-style workspace)
    };
    let at = repo
        .store()
        .get_commit(&at_id)
        .context("failed to load working-copy commit")?;
    let op_id = repo.operation().id().clone();
    workspace
        .check_out(op_id, None, &at)
        .await
        .map_err(|e| anyhow!(e))
        .context("failed to refresh working copy")?;
    Ok(())
}

/// Reload the workspace at head and refresh the working copy. Use as a finalizer
/// after a command has committed transactions.
pub async fn recover_working_copy(cwd: &std::path::Path) -> Result<()> {
    let mut loaded = crate::jj::repo::load(cwd).await?;
    let repo = loaded.repo.clone();
    recover(&mut loaded.workspace, &repo).await
}

/// Create an empty child commit on top of `head`, make it the working-copy
/// commit, and check it out on disk. Returns the new commit.
pub async fn new_on_top(
    workspace: &mut Workspace,
    repo: &Arc<ReadonlyRepo>,
    head: &CommitId,
    message: Option<&str>,
) -> Result<Commit> {
    let wc_name = workspace.workspace_name().to_owned();

    // Tree of the current working-copy commit, for the checkout's diff base.
    let old_tree = match repo.view().get_wc_commit_id(&wc_name) {
        Some(id) => Some(repo.store().get_commit(id).context("load current @")?.tree()),
        None => None,
    };

    // A new empty change shares its parent's tree.
    let head_commit = repo.store().get_commit(head).context("load stack head")?;
    let tree = head_commit.tree();

    let mut tx = repo.start_transaction();
    let new_commit = {
        let mut_repo = tx.repo_mut();
        // Inherit the stack head's author/committer identity, since jj-lib's
        // default settings carry no user.name/user.email (config-path discovery
        // lives in the jj CLI, not the library).
        let mut builder = mut_repo
            .new_commit(vec![head.clone()], tree)
            .set_author(head_commit.author().clone())
            .set_committer(head_commit.committer().clone());
        if let Some(m) = message {
            builder = builder.set_description(m);
        }
        let commit = builder
            .write()
            .await
            .map_err(|e| anyhow!(e))
            .context("failed to write new commit")?;
        mut_repo
            .edit(wc_name.clone(), &commit)
            .await
            .map_err(|e| anyhow!(e))
            .context("failed to set working-copy commit")?;
        commit
    };

    // `edit` may abandon the previous (empty) working-copy commit. If that
    // commit had descendants, they must be rebased before the transaction can
    // be committed, or jj-lib panics ("Descendants have not been rebased").
    tx.repo_mut()
        .rebase_descendants()
        .await
        .map_err(|e| anyhow!(e))
        .context("failed to rebase descendants of the abandoned working-copy commit")?;

    let new_repo = tx
        .commit("stacker: new change on top")
        .await
        .map_err(|e| anyhow!(e))
        .context("failed to commit new change")?;

    let op_id = new_repo.operation().id().clone();
    workspace
        .check_out(op_id, old_tree.as_ref(), &new_commit)
        .await
        .map_err(|e| anyhow!(e))
        .context("failed to check out new change")?;

    Ok(new_commit)
}

/// Insert an empty commit between `child` and its parent: the new commit takes
/// the parent's tree, `child` is reparented onto it, and `child`'s descendants
/// are rebased along. The new commit becomes the working-copy commit and is
/// checked out on disk. Returns the new commit.
pub async fn insert_before(
    workspace: &mut Workspace,
    repo: &Arc<ReadonlyRepo>,
    child: &CommitId,
    message: Option<&str>,
) -> Result<Commit> {
    let wc_name = workspace.workspace_name().to_owned();

    // Tree of the current working-copy commit, for the checkout's diff base.
    let old_tree = match repo.view().get_wc_commit_id(&wc_name) {
        Some(id) => Some(repo.store().get_commit(id).context("load current @")?.tree()),
        None => None,
    };

    let child_commit = repo
        .store()
        .get_commit(child)
        .context("failed to load the change to insert below")?;
    let [parent_id] = child_commit.parent_ids() else {
        bail!("cannot insert below a merge commit (stacks are linear)");
    };
    let parent_commit = repo
        .store()
        .get_commit(parent_id)
        .context("failed to load the insertion point's parent")?;
    // A new empty change shares its parent's tree.
    let tree = parent_commit.tree();

    let mut tx = repo.start_transaction();
    // Inherit the child's author/committer identity, since jj-lib's default
    // settings carry no user.name/user.email (config-path discovery lives in
    // the jj CLI, not the library).
    let mut builder = tx
        .repo_mut()
        .new_commit(vec![parent_id.clone()], tree)
        .set_author(child_commit.author().clone())
        .set_committer(child_commit.committer().clone());
    if let Some(m) = message {
        builder = builder.set_description(m);
    }
    let new_commit = builder
        .write()
        .await
        .map_err(|e| anyhow!(e))
        .context("failed to write inserted commit")?;

    // Reparent the child onto the new commit; descendants follow below.
    CommitRewriter::new(tx.repo_mut(), child_commit, vec![new_commit.id().clone()])
        .rebase()
        .await
        .map_err(|e| anyhow!(e))
        .context("failed to reparent the change above the insertion")?
        .write()
        .await
        .map_err(|e| anyhow!(e))
        .context("failed to write reparented change")?;

    tx.repo_mut()
        .edit(wc_name.clone(), &new_commit)
        .await
        .map_err(|e| anyhow!(e))
        .context("failed to set working-copy commit")?;

    // Rebase the reparented child's descendants (and anything abandoned by
    // `edit`) before committing, or jj-lib panics ("Descendants have not been
    // rebased"). Bookmarks on rewritten commits move automatically.
    tx.repo_mut()
        .rebase_descendants()
        .await
        .map_err(|e| anyhow!(e))
        .context("failed to rebase descendants of the insertion point")?;

    let new_repo = tx
        .commit("stacker: insert change")
        .await
        .map_err(|e| anyhow!(e))
        .context("failed to commit inserted change")?;

    let op_id = new_repo.operation().id().clone();
    workspace
        .check_out(op_id, old_tree.as_ref(), &new_commit)
        .await
        .map_err(|e| anyhow!(e))
        .context("failed to check out inserted change")?;

    Ok(new_commit)
}
