//! `stacker untrack <name>` — stop tracking a stack.
//!
//! Removes the stack from stacker's store and prunes orphaned change metadata.
//! Does not touch jj changes, bookmarks, or GitHub PRs — it only forgets that
//! stacker was managing them.

use anyhow::{Result, anyhow};

use crate::jj::repo;
use crate::paths;
use crate::store::Store;

pub async fn run(name: String) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let loaded = repo::load(&cwd).await?;
    let state_dir = paths::repo_state_dir(loaded.repo_path())?;
    let mut store = Store::load(&state_dir)?;

    match store.untrack_stack(&name) {
        Some(count) => {
            store.save()?;
            println!(
                "Stopped tracking stack '{name}' ({count} change(s)).\n\
                 jj changes, bookmarks, and GitHub PRs are untouched."
            );
            Ok(())
        }
        None => Err(anyhow!("no such stack: {name}")),
    }
}
