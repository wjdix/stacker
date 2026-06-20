//! Loading a jj repository in-process via `jj-lib`.
//!
//! `jj-lib`'s loader requires the workspace root directory itself (the one that
//! contains `.jj`); it does not search ancestors. We therefore walk up from the
//! current directory to find it, mirroring what the `jj` CLI does.
//!
//! A single underlying repo can be shared by several workspaces: in extra
//! workspaces `.jj/repo` is a *file* pointing at the real repo dir. `jj-lib`
//! resolves that to a canonical path, which we expose as [`LoadedRepo::repo_key`]
//! — a stable identity for the repo across all of its workspaces.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use jj_lib::config::StackedConfig;
use jj_lib::repo::{ReadonlyRepo, StoreFactories};
use jj_lib::settings::UserSettings;
use jj_lib::workspace::{Workspace, default_working_copy_factories};

/// A loaded jj workspace together with its repo view at the head operation.
pub struct LoadedRepo {
    pub workspace: Workspace,
    pub repo: Arc<ReadonlyRepo>,
}

impl LoadedRepo {
    /// Canonical path of the shared `.jj/repo` directory. Identical for every
    /// workspace backed by the same repo, so it is our cross-workspace key.
    pub fn repo_path(&self) -> &Path {
        self.workspace.repo_path()
    }
}

/// Walk up from `start` until a directory containing `.jj` is found.
pub fn find_workspace_root(start: &Path) -> Result<PathBuf> {
    let mut dir: Option<&Path> = Some(start);
    while let Some(d) = dir {
        if d.join(".jj").is_dir() {
            return Ok(d.to_path_buf());
        }
        dir = d.parent();
    }
    Err(anyhow!(
        "no jj workspace found in {} or any parent directory",
        start.display()
    ))
}

/// Build jj's library default settings. Sufficient for reading; commit creation
/// later may layer in user identity.
pub fn user_settings() -> Result<UserSettings> {
    UserSettings::from_config(StackedConfig::with_defaults())
        .context("failed to construct jj UserSettings")
}

/// Load the workspace containing `cwd` and its repo at the head operation.
pub async fn load(cwd: &Path) -> Result<LoadedRepo> {
    let root = find_workspace_root(cwd)?;
    let settings = user_settings()?;
    let workspace = Workspace::load(
        &settings,
        &root,
        &StoreFactories::default(),
        &default_working_copy_factories(),
    )
    .with_context(|| format!("failed to load jj workspace at {}", root.display()))?;
    let repo = workspace
        .repo_loader()
        .load_at_head()
        .await
        .context("failed to load jj repo at head operation")?;
    Ok(LoadedRepo { workspace, repo })
}
