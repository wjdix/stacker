//! Building an authenticated `octocrab` client.
//!
//! Token resolution precedence: `GITHUB_TOKEN`, then `GH_TOKEN`, then the
//! `gh auth token` subprocess (auth probe only — not a repo operation).

use anyhow::{Context, Result, anyhow};
use octocrab::Octocrab;

/// Resolve a GitHub token from the environment or the `gh` CLI.
pub fn resolve_token() -> Option<String> {
    for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(t) = std::env::var(var) {
            let t = t.trim().to_string();
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    let out = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .ok()?;
    if out.status.success() {
        let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    None
}

/// Build an authenticated client, or an error describing how to authenticate.
pub fn build() -> Result<Octocrab> {
    let token = resolve_token().ok_or_else(|| {
        anyhow!(
            "no GitHub token found; set GITHUB_TOKEN or GH_TOKEN, or run `gh auth login`"
        )
    })?;
    Octocrab::builder()
        .personal_token(token)
        .build()
        .context("failed to build GitHub client")
}
