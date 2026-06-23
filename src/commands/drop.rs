//! `stacker drop [stack]` — when the bottom PR is closed without merging,
//! abandon that change and restack the rest onto trunk.
//!
//! Unlike `land` (which handles a *merged* bottom, whose content is already in
//! trunk), a closed-unmerged bottom's diff is not in trunk, so we jj-abandon the
//! change — rebasing the change(s) above it onto its parent — then delegate to
//! `sync` to retarget the new bottom PR to trunk and force-push. Only runs when
//! the bottom PR is closed (unmerged); a merged bottom is redirected to `land`
//! and a still-open bottom is refused.

use std::io::Write;

use anyhow::{Result, anyhow};

use crate::commands::sync;
use crate::config::Config;
use crate::github::client;
use crate::github::pr::{self, PrState};
use crate::github::remote;
use crate::jj::{repo, rewrite, trunk};
use crate::paths;
use crate::stack;
use crate::store::Store;

/// What to do with the bottom given its current PR state.
#[derive(Debug, PartialEq, Eq)]
enum DropAction {
    /// Closed without merging — abandon it and restack.
    Proceed,
    /// Merged — the content is in trunk; use `land` instead.
    RedirectToLand,
    /// Still open — refuse; the PR should be closed first.
    RefuseOpen,
}

fn classify_drop(state: PrState) -> DropAction {
    match state {
        PrState::Closed => DropAction::Proceed,
        PrState::Merged => DropAction::RedirectToLand,
        PrState::Open | PrState::Draft => DropAction::RefuseOpen,
    }
}

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

    let trunk_id = trunk::resolve(&loaded.repo, &config.trunk, &config.remote).ok();
    let resolved = stack::resolve(&loaded.repo, &store, &name, &config.trunk, trunk_id.as_ref())?;
    let bottom = resolved
        .entries
        .first()
        .ok_or_else(|| anyhow!("stack '{name}' is empty"))?;
    let bottom_change = bottom.change_id.clone();
    let bottom_commit = bottom.commit_id.clone();
    let bottom_short = bottom.short_change_id.clone();
    let bottom_summary = bottom.summary.clone();
    let pr_number = bottom
        .pr_number
        .ok_or_else(|| anyhow!("the bottom change has no PR; nothing to drop (use `jj abandon` directly)"))?;

    let gh_repo = remote::resolve(&loaded.repo, &config.remote)?;
    let github = client::build()?;

    let state = pr::fetch_state(&github, &gh_repo, pr_number).await?;
    match classify_drop(state) {
        DropAction::RedirectToLand => {
            println!(
                "Bottom PR #{pr_number} is merged — use `stacker land` to drop a merged bottom."
            );
            return Ok(());
        }
        DropAction::RefuseOpen => {
            println!(
                "Bottom PR #{pr_number} is {} (still open). Close it on GitHub first, then re-run `stacker drop`.",
                state.label()
            );
            return Ok(());
        }
        DropAction::Proceed => {}
    }

    println!("Bottom PR #{pr_number} is closed (unmerged).");
    println!("Plan for stack '{name}':");
    println!("  • abandon bottom change {bottom_short}  {bottom_summary}");
    println!("  • rebase the change(s) above it onto '{}'", config.trunk);
    println!("  • retarget the new bottom PR to '{}' and force-push", config.trunk);
    if !yes && !confirm()? {
        println!("Aborted.");
        return Ok(());
    }

    // Abandon the bottom change; descendants rebase onto its parent (trunk).
    rewrite::abandon_change(&loaded, &bottom_commit).await?;

    // Drop the abandoned change from the stack.
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
        println!("Stack '{name}' is now empty and has been removed.");
        return Ok(());
    }
    store.save()?;

    // Restack the remainder onto trunk and update PRs. The abandon already
    // reparented onto trunk, so sync's rebase is a no-op; we skip the fetch
    // (trunk didn't move) and pass --yes since we confirmed the whole plan above.
    drop(loaded);
    println!("Restacking the remaining changes onto '{}'...", config.trunk);
    sync::run(Some(name), true, true).await
}

fn confirm() -> Result<bool> {
    print!("Proceed? [y/N] ");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim().to_lowercase().as_str(), "y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_pr_proceeds() {
        assert_eq!(classify_drop(PrState::Closed), DropAction::Proceed);
    }

    #[test]
    fn merged_pr_redirects_to_land() {
        assert_eq!(classify_drop(PrState::Merged), DropAction::RedirectToLand);
    }

    #[test]
    fn open_pr_is_refused() {
        assert_eq!(classify_drop(PrState::Open), DropAction::RefuseOpen);
        assert_eq!(classify_drop(PrState::Draft), DropAction::RefuseOpen);
    }
}
