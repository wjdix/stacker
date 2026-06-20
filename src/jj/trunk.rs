//! Resolving the trunk commit (the rebase target).
//!
//! Trunk is identified by a bookmark name (default `main`) on a remote (default
//! `origin`). We prefer the remote-tracking bookmark (`main@origin`), since that
//! reflects what has actually landed, and fall back to the local bookmark.

use anyhow::{Result, anyhow};
use jj_lib::backend::CommitId;
use jj_lib::ref_name::{RefName, RemoteName};
use jj_lib::repo::ReadonlyRepo;

/// Resolve the trunk commit id from the configured bookmark/remote.
pub fn resolve(repo: &ReadonlyRepo, bookmark: &str, remote: &str) -> Result<CommitId> {
    let view = repo.view();
    let name = RefName::new(bookmark);
    let remote_name = RemoteName::new(remote);

    // Prefer the remote-tracking bookmark.
    let remote_ref = view.get_remote_bookmark(name.to_remote_symbol(remote_name));
    if let Some(id) = remote_ref.target.as_normal() {
        return Ok(id.clone());
    }

    // Fall back to a local bookmark of the same name.
    if let Some(id) = view.get_local_bookmark(name).as_normal() {
        return Ok(id.clone());
    }

    Err(anyhow!(
        "could not resolve trunk: bookmark '{bookmark}' not found on remote '{remote}' or locally"
    ))
}
