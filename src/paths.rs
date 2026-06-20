//! Locating the global, per-repo state directory.
//!
//! Stacker keeps its metadata and config outside the repo, in a global location
//! keyed by the repo. The key is derived from the canonical shared `.jj/repo`
//! path so that every workspace of the same repo maps to the same directory.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// A stable, filesystem-safe key for a repo, derived from its canonical
/// `.jj/repo` path: `<last-path-component>-<hash>`.
pub fn repo_key(repo_path: &Path) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    repo_path.hash(&mut hasher);
    let hash = hasher.finish();

    // repo_path is `.../<name>/.jj/repo`; use `<name>` as a readable label.
    let label = repo_path
        .parent() // .jj
        .and_then(|p| p.parent()) // <name>
        .and_then(|p| p.file_name())
        .or_else(|| repo_path.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string());
    let label: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();

    format!("{label}-{hash:016x}")
}

/// The per-repo state directory, created if missing.
/// e.g. `~/Library/Application Support/stacker/<repo-key>/` on macOS.
pub fn repo_state_dir(repo_path: &Path) -> Result<PathBuf> {
    let base = dirs::data_dir()
        .context("could not determine a data directory for the current user")?
        .join("stacker")
        .join(repo_key(repo_path));
    std::fs::create_dir_all(&base)
        .with_context(|| format!("failed to create state dir {}", base.display()))?;
    Ok(base)
}
