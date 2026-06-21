//! `stacker sync [stack]` — fetch trunk, rebase the whole stack onto it, update PRs.
//!
//! By default trunk is fetched first so the rebase picks up changes that landed
//! on main (`--no-fetch` skips this). Rebase is local (jj records any conflicts
//! inside commits). After rebasing we reload and run the submit pipeline, which
//! force-pushes moved bookmarks, retargets bases, refreshes nav comments, and
//! skips conflicted changes (and everything above) with a non-zero exit.

use anyhow::{Result, anyhow};

use crate::commands::submit;
use crate::config::Config;
use crate::jj::{fetch, repo, rewrite, trunk};
use crate::paths;
use crate::stack;
use crate::store::Store;

pub async fn run(stack_name: Option<String>, yes: bool, no_fetch: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    crate::jj::snapshot::snapshot_working_copy(&cwd);
    let mut loaded = repo::load(&cwd).await?;
    let state_dir = paths::repo_state_dir(loaded.repo_path())?;
    let config = Config::load(&state_dir)?;
    let store = Store::load(&state_dir)?;

    let name = match stack_name {
        Some(n) => n,
        None => stack::infer_stack_name(&loaded, &store)?,
    };

    // Fetch trunk so the rebase picks up changes that landed on main.
    if !no_fetch {
        println!("Fetching '{}' from '{}'...", config.trunk, config.remote);
        fetch::fetch_trunk(&loaded, &config.remote, &config.trunk).await?;
        drop(loaded);
        loaded = repo::load(&cwd).await?;
    }

    let trunk_id = trunk::resolve(&loaded.repo, &config.trunk, &config.remote)?;
    let resolved = stack::resolve(&loaded.repo, &store, &name, &config.trunk, Some(&trunk_id))?;
    let bottom = resolved
        .entries
        .first()
        .map(|e| e.commit_id.clone())
        .ok_or_else(|| anyhow!("stack '{name}' has no changes"))?;

    let rebased = rewrite::rebase_stack_onto(&loaded, &bottom, &trunk_id).await?;
    if rebased {
        println!("Rebased stack '{name}' onto '{}'.", config.trunk);
    } else {
        println!("Stack '{name}' is already based on '{}'.", config.trunk);
    }

    // Reload to observe the rebased state, then run the submit pipeline.
    drop(loaded);
    let loaded = repo::load(&cwd).await?;
    let mut store = Store::load(&state_dir)?;
    let result = submit::submit_stack(&loaded, &config, &mut store, &name, yes).await;
    drop(loaded);
    submit::finalize(&cwd).await;
    result
}
