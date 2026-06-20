//! Pushing bookmarks to a git remote via `jj-lib`.
//!
//! `jj_lib::git::push_refs` is the native push path. It is lease-safe: the
//! `before` target (the local remote-tracking position) is verified during push
//! negotiation, so a force-push won't clobber unexpected remote movement.
//! Internally jj-lib spawns `git` as a subprocess — that is jj's own design, and
//! it means credentials flow through git's normal mechanisms.

use std::io;

use anyhow::{Context, Result, anyhow};
use jj_lib::backend::CommitId;
use jj_lib::git::{
    self, GitProgress, GitPushOptions, GitPushRefTargets, GitPushStats, GitSidebandLineTerminator,
    GitSubprocessCallback, GitSubprocessOptions,
};
use jj_lib::merge::Diff;
use jj_lib::op_store::RefTarget;
use jj_lib::ref_name::{RefName, RemoteName};

use crate::jj::repo::LoadedRepo;

/// Forwards git sideband output (e.g. "Create a pull request" hints) to stderr.
pub(crate) struct SidebandCallback;

impl GitSubprocessCallback for SidebandCallback {
    fn needs_progress(&self) -> bool {
        false
    }
    fn progress(&mut self, _progress: &GitProgress) -> io::Result<()> {
        Ok(())
    }
    fn local_sideband(
        &mut self,
        message: &[u8],
        _term: Option<GitSidebandLineTerminator>,
    ) -> io::Result<()> {
        eprint!("{}", String::from_utf8_lossy(message));
        Ok(())
    }
    fn remote_sideband(
        &mut self,
        message: &[u8],
        _term: Option<GitSidebandLineTerminator>,
    ) -> io::Result<()> {
        eprint!("remote: {}", String::from_utf8_lossy(message));
        Ok(())
    }
}

/// Move the given local bookmarks to `target` commits and push them to `remote`
/// in one transaction. Returns push stats (pushed / rejected refs).
pub async fn push_bookmarks(
    loaded: &LoadedRepo,
    remote: &str,
    bookmarks: &[(String, CommitId)],
) -> Result<GitPushStats> {
    let remote_name = RemoteName::new(remote);

    // Expected remote positions (lease) come from the current remote-tracking refs.
    let view = loaded.repo.view();
    let mut targets = GitPushRefTargets::default();
    for (name, new_id) in bookmarks {
        let ref_name = RefName::new(name);
        let before = view
            .get_remote_bookmark(ref_name.to_remote_symbol(remote_name))
            .target
            .as_normal()
            .cloned();
        targets
            .bookmarks
            .push((name.clone().into(), Diff::new(before, Some(new_id.clone()))));
    }

    let options = GitSubprocessOptions::from_settings(loaded.workspace.settings())
        .map_err(|e| anyhow!(e))
        .context("failed to read git subprocess options")?;
    let push_options = GitPushOptions::default();
    let mut callback = SidebandCallback;

    let mut tx = loaded.repo.start_transaction();
    {
        let mut_repo = tx.repo_mut();
        for (name, new_id) in bookmarks {
            mut_repo.set_local_bookmark_target(RefName::new(name), RefTarget::normal(new_id.clone()));
        }
    }
    let stats = git::push_refs(
        tx.repo_mut(),
        options,
        remote_name,
        &targets,
        &mut callback,
        &push_options,
    )
    .map_err(|e| anyhow!(e))
    .context("git push failed")?;

    tx.commit("stacker: push bookmarks")
        .await
        .map_err(|e| anyhow!(e))
        .context("failed to commit push")?;

    Ok(stats)
}
