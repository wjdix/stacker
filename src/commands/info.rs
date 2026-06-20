//! `stacker info` — Stage 1 smoke command.
//!
//! Verifies the foundations end-to-end: load the repo in-process, resolve the
//! cross-workspace repo key, load config and the metadata store, and resolve the
//! trunk commit.

use anyhow::Result;
use jj_lib::object_id::ObjectId;

use crate::config::Config;
use crate::jj::{repo, trunk};
use crate::paths;
use crate::store::Store;

pub async fn run() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let loaded = repo::load(&cwd).await?;

    let repo_path = loaded.repo_path();
    let state_dir = paths::repo_state_dir(repo_path)?;
    let config = Config::load(&state_dir)?;
    let store = Store::load(&state_dir)?;

    println!("Repo path:   {}", repo_path.display());
    println!("Repo key:    {}", paths::repo_key(repo_path));
    println!("State dir:   {}", state_dir.display());
    println!(
        "Config:      trunk={} remote={} prefix={:?}",
        config.trunk, config.remote, config.bookmark_prefix
    );

    match trunk::resolve(&loaded.repo, &config.trunk, &config.remote) {
        Ok(id) => println!("Trunk:       {} ({})", config.trunk, id.hex()),
        Err(e) => println!("Trunk:       <unresolved: {e}>"),
    }

    println!(
        "Store:       {} stack(s), {} tracked change(s)",
        store.stacks.len(),
        store.changes.len()
    );

    Ok(())
}
