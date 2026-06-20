//! `stacker land [stack]` — when the bottom PR is merged, drop it and restack.
//!
//! Detects that the bottom PR is merged, fetches trunk so it includes the merge,
//! drops the bottom change from the stack, then delegates to `sync` to rebase the
//! remaining changes onto the updated trunk and retarget the new bottom PR.

use anyhow::{Result, anyhow};

use crate::commands::sync;
use crate::config::Config;
use crate::github::client;
use crate::github::pr::{self, PrState};
use crate::github::remote;
use crate::jj::{fetch, repo};
use crate::paths;
use crate::stack;
use crate::store::Store;

pub async fn run(stack_name: Option<String>, yes: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    crate::jj::snapshot::snapshot_working_copy(&cwd);
    let loaded = repo::load(&cwd).await?;
    let state_dir = paths::repo_state_dir(loaded.repo_path())?;
    let config = Config::load(&state_dir)?;
    let mut store = Store::load(&state_dir)?;

    let name = match stack_name {
        Some(n) => n,
        None => stack::infer_stack_name(&loaded, &store)?,
    };

    let resolved = stack::resolve(&loaded.repo, &store, &name, &config.trunk)?;
    let bottom = resolved
        .entries
        .first()
        .ok_or_else(|| anyhow!("stack '{name}' is empty"))?;
    let bottom_change = bottom.change_id.clone();
    let pr_number = bottom
        .pr_number
        .ok_or_else(|| anyhow!("the bottom change has no PR; nothing to land"))?;

    let gh_repo = remote::resolve(&loaded.repo, &config.remote)?;
    let github = client::build()?;

    let state = pr::fetch_state(&github, &gh_repo, pr_number).await?;
    if state != PrState::Merged {
        println!(
            "Bottom PR #{pr_number} is {} (not merged). Nothing to land.",
            state.label()
        );
        return Ok(());
    }
    println!("Bottom PR #{pr_number} is merged.");

    // Refresh trunk so it contains the merged change before we rebase onto it.
    println!("Fetching '{}' from '{}'...", config.trunk, config.remote);
    fetch::fetch_trunk(&loaded, &config.remote, &config.trunk).await?;

    // Drop the merged change from the stack.
    store.drop_change(&name, &bottom_change);

    let now_empty = store
        .stacks
        .get(&name)
        .map(|s| s.members.is_empty())
        .unwrap_or(true);
    if now_empty {
        store.stacks.remove(&name);
        store.save()?;
        drop(loaded);
        crate::commands::submit::finalize(&cwd).await;
        println!("Stack '{name}' is fully landed and has been removed.");
        return Ok(());
    }
    store.save()?;

    // Rebase the remaining changes onto the refreshed trunk and update PRs.
    // We already fetched trunk above, so tell sync not to fetch again.
    drop(loaded);
    println!("Restacking the remaining changes onto '{}'...", config.trunk);
    sync::run(Some(name), yes, true).await
}
