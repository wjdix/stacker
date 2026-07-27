//! `stacker abandon [stack]` — abandon every change in a stack and stop
//! tracking it.
//!
//! The whole-stack teardown, for work that isn't going to land: jj-abandons all
//! live member changes (discarding their diffs), deletes their local bookmarks,
//! and forgets the stack. Refuses while any member PR is still open or draft —
//! close (or merge) them on GitHub first. Closed/merged PRs and remote branches
//! are left untouched. Unlike `untrack` (which only forgets metadata), this
//! rewrites the jj graph; the abandon is recoverable via `jj undo`. A stack
//! with no live changes is simply removed from the store.

use std::io::Write;

use anyhow::Result;

use crate::config::Config;
use crate::github::client;
use crate::github::pr::{self, PrState};
use crate::github::remote;
use crate::jj::{repo, rewrite, trunk};
use crate::paths;
use crate::stack;
use crate::store::Store;

/// PR numbers that block abandoning: any PR still open or draft.
fn blocking_prs(prs: &[(u64, PrState)]) -> Vec<u64> {
    prs.iter()
        .filter(|(_, state)| matches!(state, PrState::Open | PrState::Draft))
        .map(|(number, _)| *number)
        .collect()
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

    // Nothing live to abandon — just forget the stack.
    if resolved.entries.is_empty() {
        store.untrack_stack(&name);
        store.save()?;
        println!("Stack '{name}' has no live changes; stopped tracking it.");
        return Ok(());
    }

    // An open PR means the work is still in review; refuse until it's resolved
    // on GitHub. Closed and merged PRs are safe to leave behind.
    let with_prs: Vec<u64> = resolved.entries.iter().filter_map(|e| e.pr_number).collect();
    if !with_prs.is_empty() {
        let gh_repo = remote::resolve(&loaded.repo, &config.remote)?;
        let github = client::build()?;
        let mut states = Vec::with_capacity(with_prs.len());
        for number in with_prs {
            states.push((number, pr::fetch_state(&github, &gh_repo, number).await?));
        }
        let blockers = blocking_prs(&states);
        if !blockers.is_empty() {
            println!("Stack '{name}' still has open PR(s):");
            for (number, state) in &states {
                if blockers.contains(number) {
                    println!("  • PR #{number} ({})", state.label());
                }
            }
            println!("Close (or merge) them on GitHub first, then re-run `stacker abandon`.");
            return Ok(());
        }
    }

    let mut bookmarks: Vec<String> = resolved
        .entries
        .iter()
        .filter_map(|e| e.bookmark.clone())
        .collect();
    bookmarks.dedup();

    println!("Plan for stack '{name}':");
    for entry in &resolved.entries {
        println!(
            "  • abandon {}  {}",
            entry.short_change_id, entry.summary
        );
    }
    if !bookmarks.is_empty() {
        println!("  • delete local bookmark(s): {}", bookmarks.join(", "));
    }
    println!("  • stop tracking stack '{name}'");
    println!("Closed/merged PRs and remote branches are untouched; recover with `jj undo`.");
    if !yes && !confirm()? {
        println!("Aborted.");
        return Ok(());
    }

    let targets: Vec<_> = resolved.entries.iter().map(|e| e.commit_id.clone()).collect();
    rewrite::abandon_stack(&loaded, &targets, &bookmarks).await?;

    store.untrack_stack(&name);
    store.save()?;

    drop(loaded);
    crate::commands::submit::finalize(&cwd).await;
    println!(
        "Abandoned {} change(s) and stopped tracking stack '{name}'.",
        targets.len()
    );
    Ok(())
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
    fn open_and_draft_prs_block() {
        let states = vec![
            (1, PrState::Open),
            (2, PrState::Draft),
            (3, PrState::Closed),
        ];
        assert_eq!(blocking_prs(&states), vec![1, 2]);
    }

    #[test]
    fn closed_and_merged_prs_do_not_block() {
        let states = vec![(4, PrState::Closed), (5, PrState::Merged)];
        assert!(blocking_prs(&states).is_empty());
    }

    #[test]
    fn no_prs_do_not_block() {
        assert!(blocking_prs(&[]).is_empty());
    }
}
