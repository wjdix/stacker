//! Per-repo stacker configuration.
//!
//! Stored as `config.toml` in the repo's global state dir. All fields have
//! defaults, so a missing file is equivalent to all defaults.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

fn default_trunk() -> String {
    "main".to_string()
}
fn default_remote() -> String {
    "origin".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Bookmark name of trunk (the rebase target). Default `main`.
    pub trunk: String,
    /// Git remote to push to and resolve trunk against. Default `origin`.
    pub remote: String,
    /// Prefix prepended to generated bookmark names, e.g. `wd/`. Empty by default.
    pub bookmark_prefix: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            trunk: default_trunk(),
            remote: default_remote(),
            bookmark_prefix: String::new(),
        }
    }
}

impl Config {
    const FILE_NAME: &'static str = "config.toml";

    /// Load config from `state_dir/config.toml`, or defaults if absent.
    pub fn load(state_dir: &Path) -> Result<Self> {
        let path = state_dir.join(Self::FILE_NAME);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let cfg = toml::from_str(&text)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(cfg)
    }
}
