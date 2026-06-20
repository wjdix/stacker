//! `stacker create <name> [--workspace <path>] [-m msg]` — start a NEW stack.
//!
//! Creates a fresh empty change on top of trunk and registers it as stack
//! `<name>` (its first change). Without `--workspace` the change is created in
//! the current workspace (moving `@`). With `--workspace <path>` it is created
//! in a new jj workspace at that path, leaving the current workspace untouched.
//!
//! The change creation (and on-disk checkout) is delegated to `jj new` /
//! `jj workspace add` — the same justified shell-out as snapshotting. This keeps
//! jj's correct author identity and working-copy handling, which jj-lib's
//! library API does not provide ergonomically.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use jj_lib::object_id::ObjectId;
use jj_lib::ref_name::WorkspaceName;
use jj_lib::repo::Repo;

use crate::config::Config;
use crate::jj::{repo, trunk};
use crate::paths;
use crate::store::{Stack, Store};

pub async fn run(name: String, workspace: Option<PathBuf>, message: Option<String>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let loaded = repo::load(&cwd).await?;
    let state_dir = paths::repo_state_dir(loaded.repo_path())?;
    let config = Config::load(&state_dir)?;
    let store = Store::load(&state_dir)?;

    if store.stacks.contains_key(&name) {
        return Err(anyhow!("stack '{name}' already exists"));
    }
    let trunk_id = trunk::resolve(&loaded.repo, &config.trunk, &config.remote)?;
    let trunk_hex = trunk_id.hex();
    drop(loaded); // release the read view before invoking jj

    // Create the new change (jj handles author identity + working-copy checkout).
    let ws_name = match &workspace {
        Some(path) => Some(workspace_add(&cwd, path, &trunk_hex, message.as_deref())?),
        None => {
            new_change(&cwd, &trunk_hex, message.as_deref())?;
            None
        }
    };

    // Reload the (shared) repo and read the new change's id.
    let loaded = repo::load(&cwd).await?;
    let commit_id = match &ws_name {
        Some(n) => loaded.repo.view().get_wc_commit_id(WorkspaceName::new(n)),
        None => loaded
            .repo
            .view()
            .get_wc_commit_id(loaded.workspace.workspace_name()),
    }
    .ok_or_else(|| anyhow!("could not find the working-copy commit for the new change"))?
    .clone();
    let change_id = loaded
        .repo
        .store()
        .get_commit(&commit_id)
        .context("failed to load the new change")?
        .change_id()
        .hex();

    let mut store = Store::load(&state_dir)?;
    store
        .stacks
        .insert(name.clone(), Stack { members: vec![change_id.clone()] });
    store.save()?;

    let short: String = change_id.chars().take(8).collect();
    match &ws_name {
        Some(n) => println!(
            "Created stack '{name}' (change {short}) in new workspace '{n}'. Edit there, then `stacker submit {name}`."
        ),
        None => println!(
            "Created stack '{name}' (change {short}) on top of trunk. Edit, then `stacker submit {name}`."
        ),
    }
    Ok(())
}

/// `jj new <trunk>` in the current workspace.
fn new_change(cwd: &Path, trunk_hex: &str, message: Option<&str>) -> Result<()> {
    let mut cmd = std::process::Command::new("jj");
    cmd.current_dir(cwd).arg("new");
    if let Some(m) = message {
        cmd.arg("-m").arg(m);
    }
    cmd.arg(trunk_hex);
    let status = cmd.status().context("failed to run `jj new`")?;
    if !status.success() {
        return Err(anyhow!("`jj new` failed"));
    }
    Ok(())
}

/// `jj workspace add --name <derived> -r <trunk> <path>`; returns the workspace name.
fn workspace_add(
    cwd: &Path,
    dest: &Path,
    trunk_hex: &str,
    message: Option<&str>,
) -> Result<String> {
    let ws_name = dest
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .ok_or_else(|| anyhow!("workspace path has no final component: {}", dest.display()))?;

    let mut cmd = std::process::Command::new("jj");
    cmd.current_dir(cwd)
        .arg("workspace")
        .arg("add")
        .arg("--name")
        .arg(&ws_name)
        .arg("-r")
        .arg(trunk_hex);
    if let Some(m) = message {
        cmd.arg("-m").arg(m);
    }
    cmd.arg(dest);
    let status = cmd.status().context("failed to run `jj workspace add`")?;
    if !status.success() {
        return Err(anyhow!("`jj workspace add` failed"));
    }
    Ok(ws_name)
}
