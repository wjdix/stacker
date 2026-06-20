//! Fetching trunk from the remote via `jj-lib`, so `land` rebases onto a trunk
//! that already contains the just-merged bottom change.

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow};
use jj_lib::git::{
    self, GitFetch, GitFetchRefExpression, GitImportOptions, GitSubprocessOptions,
};
use jj_lib::ref_name::RemoteName;
use jj_lib::str_util::StringExpression;

use crate::jj::push::SidebandCallback;
use crate::jj::repo::LoadedRepo;

/// Fetch the `trunk` bookmark from `remote`, updating the remote-tracking ref.
pub async fn fetch_trunk(loaded: &LoadedRepo, remote: &str, trunk: &str) -> Result<()> {
    let remote_name = RemoteName::new(remote);
    let options = GitSubprocessOptions::from_settings(loaded.workspace.settings())
        .map_err(|e| anyhow!(e))
        .context("failed to read git subprocess options")?;

    let expr = GitFetchRefExpression {
        bookmark: StringExpression::exact(trunk),
        tag: StringExpression::none(),
    };
    let expanded = git::expand_fetch_refspecs(remote_name, expr)
        .map_err(|e| anyhow!(e))
        .context("failed to expand fetch refspecs")?;

    let import_options = GitImportOptions {
        auto_local_bookmark: false,
        abandon_unreachable_commits: false,
        remote_auto_track_bookmarks: HashMap::new(),
    };

    let mut tx = loaded.repo.start_transaction();
    {
        let mut fetch = GitFetch::new(tx.repo_mut(), options, &import_options)
            .map_err(|e| anyhow!(e))
            .context("failed to start git fetch")?;
        let mut callback = SidebandCallback;
        fetch
            .fetch(remote_name, expanded, &mut callback, None, None)
            .map_err(|e| anyhow!(e))
            .context("git fetch failed")?;
        fetch
            .import_refs()
            .await
            .map_err(|e| anyhow!(e))
            .context("failed to import fetched refs")?;
    }
    tx.commit("stacker: fetch trunk")
        .await
        .map_err(|e| anyhow!(e))
        .context("failed to commit fetch")?;
    Ok(())
}
