//! `stacker status` — show each tracked stack, ordered, with live PR state.

use anyhow::Result;
use octocrab::Octocrab;

use crate::config::Config;
use crate::github::pr::{self, PrState};
use crate::github::remote::{self, GitHubRepo};
use crate::github::client;
use crate::jj::{repo, trunk};
use crate::paths;
use crate::stack::{self, ResolvedStack};
use crate::store::Store;

pub async fn run() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let loaded = repo::load(&cwd).await?;
    let state_dir = paths::repo_state_dir(loaded.repo_path())?;
    let config = Config::load(&state_dir)?;
    let mut store = Store::load(&state_dir)?;

    if store.stacks.is_empty() {
        println!("No stacks tracked yet. Create one with `stacker create` or adopt with `stacker track`.");
        return Ok(());
    }

    // Best-effort GitHub wiring; status still works offline (PR state shown as "?").
    let gh_repo = remote::resolve(&loaded.repo, &config.remote).ok();
    let client = client::build().ok();
    if client.is_none() {
        eprintln!("note: no GitHub auth; PR state will be shown as '?'");
    }
    if gh_repo.is_none() {
        eprintln!("note: could not resolve GitHub repo from remote '{}'.", config.remote);
    }

    let trunk_id = trunk::resolve(&loaded.repo, &config.trunk, &config.remote).ok();
    let names: Vec<String> = store.stacks.keys().cloned().collect();
    let mut store_changed = false;
    for name in &names {
        // Discover changes appended by other tools and adopt their PRs.
        let gh = match (&client, &gh_repo) {
            (Some(c), Some(r)) => Some((c, r)),
            _ => None,
        };
        let (resolved, changed) = stack::refresh_and_resolve(
            &loaded,
            &mut store,
            name,
            &config.trunk,
            trunk_id.as_ref(),
            gh,
        )
        .await?;
        store_changed |= changed;

        print_stack(&resolved, client.as_ref(), gh_repo.as_ref()).await;
        println!();
    }
    if store_changed {
        store.save()?;
    }

    Ok(())
}

async fn print_stack(
    stack: &ResolvedStack,
    client: Option<&Octocrab>,
    gh_repo: Option<&GitHubRepo>,
) {
    println!("Stack: {}  ({} change(s))", stack.name, stack.entries.len());

    // Print head-first so trunk sits at the bottom, like a stack.
    for entry in stack.entries.iter().rev() {
        let bookmark = entry.bookmark.as_deref().unwrap_or("(no bookmark)");

        let pr_part = match entry.pr_number {
            Some(n) => {
                let state = fetch_state(client, gh_repo, n).await;
                format!("PR #{n} [{state}]")
            }
            None => "(no PR)".to_string(),
        };

        let mut flags = String::new();
        if entry.has_conflict {
            flags.push_str("  ⚠ conflict");
        }
        if entry.divergent {
            flags.push_str("  ⚠ divergent");
        }

        println!(
            "  ● {}  {:<28}  {:<22}  {:<18} {}",
            entry.short_change_id,
            truncate(&entry.summary, 28),
            bookmark,
            pr_part,
            flags
        );
    }
    println!("  └─ base: {}", stack.entries.first().map(|e| e.expected_base.as_str()).unwrap_or("-"));
    if !stack.missing.is_empty() {
        println!(
            "  ({} tracked change(s) no longer in the graph — will be pruned on next submit)",
            stack.missing.len()
        );
    }
}

async fn fetch_state(
    client: Option<&Octocrab>,
    gh_repo: Option<&GitHubRepo>,
    number: u64,
) -> String {
    match (client, gh_repo) {
        (Some(c), Some(r)) => match pr::fetch_state(c, r, number).await {
            Ok(s) => state_label(s).to_string(),
            Err(_) => "?".to_string(),
        },
        _ => "?".to_string(),
    }
}

fn state_label(s: PrState) -> &'static str {
    s.label()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
