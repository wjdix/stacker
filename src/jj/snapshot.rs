//! Snapshotting the working copy before a content-affecting command.
//!
//! jj normally snapshots the working copy (folding on-disk edits into `@`) at the
//! start of every CLI command. stacker works through `jj-lib`, which does *not*
//! do this, and faithfully reimplementing snapshot (gitignore, auto-track,
//! file-size limits, fsmonitor) would mean duplicating logic that lives in the
//! jj CLI. So before commands that read or rewrite `@`'s content we let jj
//! snapshot once, via a single quiet invocation.
//!
//! This is the one place we intentionally shell out to `jj`: it's a one-shot
//! ~10ms cost on heavyweight commands (submit/sync/land also do network IO), and
//! it reuses jj's own correct snapshot logic. The hot read-only path
//! (`status`, etc.) stays pure `jj-lib`. Best-effort: a missing or failing `jj`
//! is warned about, not fatal.

use std::path::Path;

/// Run a single quiet `jj` command to snapshot the working copy into `@`.
pub fn snapshot_working_copy(cwd: &Path) {
    let result = std::process::Command::new("jj")
        .args(["log", "--no-graph", "-r", "@", "-T", ""])
        .current_dir(cwd)
        .output();
    match result {
        Ok(out) if out.status.success() => {}
        Ok(out) => eprintln!(
            "warning: couldn't snapshot the working copy via jj; uncommitted edits to @ may be excluded\n  {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => eprintln!(
            "warning: couldn't run jj to snapshot the working copy ({e}); uncommitted edits to @ may be excluded"
        ),
    }
}
