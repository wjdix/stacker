//! Micro-benchmark: open the repo + read local bookmarks N times in-process via
//! `jj-lib`, the same way stacker does. Compare against running `jj bookmark
//! list` N times (each of which is a separate process load).
//!
//! Usage: run from inside a jj workspace:
//!   cargo run --release --example loadbench -- [N]

use std::path::{Path, PathBuf};
use std::time::Instant;

use jj_lib::config::StackedConfig;
use jj_lib::repo::StoreFactories;
use jj_lib::settings::UserSettings;
use jj_lib::workspace::{Workspace, default_working_copy_factories};

fn find_workspace_root(start: &Path) -> PathBuf {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join(".jj").is_dir() {
            return d.to_path_buf();
        }
        dir = d.parent();
    }
    panic!("no jj workspace found");
}

#[tokio::main]
async fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);

    let cwd = std::env::current_dir().unwrap();
    let root = find_workspace_root(&cwd);
    let settings = UserSettings::from_config(StackedConfig::with_defaults()).unwrap();

    // Warm up (filesystem caches, etc.).
    load_and_count(&settings, &root).await;

    let start = Instant::now();
    let mut total = 0usize;
    for _ in 0..n {
        total += load_and_count(&settings, &root).await;
    }
    let elapsed = start.elapsed();

    eprintln!(
        "jj-lib: {n} iterations in {:.3?}  =>  {:.3} ms/op  (counted {total} bookmarks total)",
        elapsed,
        elapsed.as_secs_f64() * 1000.0 / n as f64
    );
}

/// Open the workspace + repo at head and count local bookmarks (a real read).
async fn load_and_count(settings: &UserSettings, root: &Path) -> usize {
    let workspace = Workspace::load(
        settings,
        root,
        &StoreFactories::default(),
        &default_working_copy_factories(),
    )
    .unwrap();
    let repo = workspace.repo_loader().load_at_head().await.unwrap();
    repo.view().local_bookmarks().count()
}
