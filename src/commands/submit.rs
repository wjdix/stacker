//! `stacker submit` — create/update one PR per bookmark with chained bases.
//!
//! Processes a stack bottom→top. The bottom PR targets trunk; each higher PR
//! targets the bookmark below it. Changes containing conflicts (and everything
//! above them) are skipped, reported, and cause a non-zero exit. A plan is shown
//! and confirmed before any push or API call (unless `--yes`).

use std::io::Write;

use anyhow::{Result, anyhow};
use jj_lib::backend::CommitId;

use crate::config::Config;
use crate::github::client;
use crate::github::nav_comment::{self, NavRow};
use crate::github::pr;
use crate::github::remote::{self, GitHubRepo};
use crate::jj::bookmarks;
use crate::jj::push;
use crate::jj::repo::{self, LoadedRepo};
use crate::jj::trunk;
use crate::paths;
use crate::stack::{self, ResolvedStack};
use crate::store::Store;

/// What we'll do to a change's PR.
enum PrAction {
    Create,
    Retarget { to: String },
    BaseUnchanged,
}

/// A single planned submission (one submittable change).
struct Item {
    change_id: String,
    short: String,
    summary: String,
    commit_id: CommitId,
    bookmark: String,
    bookmark_is_new: bool,
    base: String,
    pr_number: Option<u64>,
    action: PrAction,
}

/// The whole plan: submittable items plus skipped (conflicted) changes.
struct Plan {
    stack_name: String,
    items: Vec<Item>,
    skipped: Vec<(String, String)>, // (short change-id, summary)
}

pub async fn run(stack_name: Option<String>, yes: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    crate::jj::snapshot::snapshot_working_copy(&cwd);
    let loaded = repo::load(&cwd).await?;
    let state_dir = paths::repo_state_dir(loaded.repo_path())?;
    let config = Config::load(&state_dir)?;
    let mut store = Store::load(&state_dir)?;

    let name = match stack_name {
        Some(n) => n,
        None => stack::infer_stack_name(&loaded, &store)?,
    };
    let result = submit_stack(&loaded, &config, &mut store, &name, yes).await;
    drop(loaded);
    finalize(&cwd).await;
    result
}

/// Refresh the working copy after the command's transactions; best-effort.
pub(crate) async fn finalize(cwd: &std::path::Path) {
    if let Err(e) = crate::jj::workspace_ops::recover_working_copy(cwd).await {
        eprintln!(
            "warning: working copy may be stale; run `jj workspace update-stale` ({e})"
        );
    }
}

/// Resolve, plan, confirm, and execute a submission for one named stack.
/// Reused by `sync` after it rebases. `store` is persisted on success.
pub async fn submit_stack(
    loaded: &LoadedRepo,
    config: &Config,
    store: &mut Store,
    name: &str,
    yes: bool,
) -> Result<()> {
    let trunk_id = trunk::resolve(&loaded.repo, &config.trunk, &config.remote).ok();
    let resolved = stack::resolve(&loaded.repo, store, name, &config.trunk, trunk_id.as_ref())?;
    // Self-heal: forget members that have landed or no longer resolve.
    let stale: Vec<String> = resolved
        .missing
        .iter()
        .chain(resolved.landed.iter())
        .cloned()
        .collect();
    if !stale.is_empty() {
        store.prune_stack_members(name, &stale);
    }
    let gh_repo = remote::resolve(&loaded.repo, &config.remote)?;

    let plan = build_plan(loaded, config, &resolved, name, store);
    print_plan(&plan, &gh_repo, &config.remote);

    if plan.items.is_empty() {
        if plan.skipped.is_empty() {
            println!("Nothing to submit.");
            return Ok(());
        }
        return Err(conflict_error(&plan));
    }

    if !yes && !confirm()? {
        println!("Aborted.");
        return Ok(());
    }

    let client = client::build()?;
    execute(loaded, config, &gh_repo, &client, &plan, store).await?;
    store.save()?;

    println!("\nDone. Submitted {} change(s).", plan.items.len());
    if !plan.skipped.is_empty() {
        return Err(conflict_error(&plan));
    }
    Ok(())
}

/// Build the plan: assign bookmarks, compute chained bases, and classify the PR
/// action, stopping at the first conflicted change.
fn build_plan(
    loaded: &LoadedRepo,
    config: &Config,
    resolved: &ResolvedStack,
    name: &str,
    store: &Store,
) -> Plan {
    let mut existing = bookmarks::existing_names(&loaded.repo);
    let mut items = Vec::new();
    let mut skipped = Vec::new();

    let mut hit_conflict = false;
    let mut prev_bookmark: Option<String> = None;

    for entry in &resolved.entries {
        if hit_conflict || entry.has_conflict {
            hit_conflict = true;
            skipped.push((entry.short_change_id.clone(), entry.summary.clone()));
            continue;
        }

        let (bookmark, is_new) = match &entry.bookmark {
            Some(b) => (b.clone(), false),
            None => {
                let b = bookmarks::generate_name(&config.bookmark_prefix, &entry.summary, &existing);
                existing.insert(b.clone());
                (b, true)
            }
        };

        let base = prev_bookmark
            .clone()
            .unwrap_or_else(|| config.trunk.clone());

        let stored_base = store
            .changes
            .get(&entry.change_id)
            .and_then(|m| m.base_branch.clone());
        let action = match entry.pr_number {
            None => PrAction::Create,
            Some(_) if stored_base.as_deref() != Some(base.as_str()) => {
                PrAction::Retarget { to: base.clone() }
            }
            Some(_) => PrAction::BaseUnchanged,
        };

        items.push(Item {
            change_id: entry.change_id.clone(),
            short: entry.short_change_id.clone(),
            summary: entry.summary.clone(),
            commit_id: entry.commit_id.clone(),
            bookmark: bookmark.clone(),
            bookmark_is_new: is_new,
            base,
            pr_number: entry.pr_number,
            action,
        });
        prev_bookmark = Some(bookmark);
    }

    Plan {
        stack_name: name.to_string(),
        items,
        skipped,
    }
}

fn print_plan(plan: &Plan, gh_repo: &GitHubRepo, remote: &str) {
    println!(
        "Plan for stack '{}'  →  {}/{}  (remote '{}')\n",
        plan.stack_name, gh_repo.owner, gh_repo.repo, remote
    );
    // Bottom→top is the submission order; show it that way.
    for (i, item) in plan.items.iter().enumerate() {
        let n = i + 1;
        let push_note = if item.bookmark_is_new {
            "create+push bookmark"
        } else {
            "push bookmark"
        };
        let pr_note = match (&item.action, item.pr_number) {
            (PrAction::Create, _) => "create draft PR".to_string(),
            (PrAction::Retarget { to }, Some(num)) => format!("retarget PR #{num} → base {to}"),
            (PrAction::BaseUnchanged, Some(num)) => format!("update PR #{num}"),
            (PrAction::Retarget { to }, None) => format!("retarget → base {to}"),
            (PrAction::BaseUnchanged, None) => "update PR".to_string(),
        };
        println!(
            "  {n}. {} {}  [{}]\n       base: {}  |  {}  |  {}",
            item.short,
            truncate(&item.summary, 40),
            item.bookmark,
            item.base,
            push_note,
            pr_note,
        );
    }
    if !plan.skipped.is_empty() {
        println!("\n  ⚠ Skipped (conflicts at or above):");
        for (short, summary) in &plan.skipped {
            println!("     - {short} {}", truncate(summary, 40));
        }
    }
    println!();
}

async fn execute(
    loaded: &LoadedRepo,
    config: &Config,
    gh_repo: &GitHubRepo,
    client: &octocrab::Octocrab,
    plan: &Plan,
    store: &mut Store,
) -> Result<()> {
    // 1. Push every submittable bookmark (sets local target + force-with-lease push).
    let pushes: Vec<(String, CommitId)> = plan
        .items
        .iter()
        .map(|i| (i.bookmark.clone(), i.commit_id.clone()))
        .collect();
    println!("Pushing {} bookmark(s) to '{}'...", pushes.len(), config.remote);
    let stats = push::push_bookmarks(loaded, &config.remote, &pushes).await?;
    if !stats.rejected.is_empty() || !stats.remote_rejected.is_empty() {
        for (name, why) in stats.rejected.iter().chain(stats.remote_rejected.iter()) {
            eprintln!(
                "  rejected {}: {}",
                name.as_str(),
                why.as_deref().unwrap_or("(no reason)")
            );
        }
        return Err(anyhow!("one or more bookmarks were rejected on push"));
    }

    // 2. Create or retarget PRs, persisting numbers/bookmarks/bases.
    for item in &plan.items {
        let meta = store.changes.entry(item.change_id.clone()).or_default();
        meta.bookmark = Some(item.bookmark.clone());

        // Discover an existing PR even if we don't have it stored, to avoid dupes.
        let existing = match item.pr_number {
            Some(n) => Some(n),
            None => pr::find_by_head(client, gh_repo, &item.bookmark).await?,
        };

        let number = match existing {
            Some(n) => {
                let stored_base = meta.base_branch.clone();
                if stored_base.as_deref() != Some(item.base.as_str()) {
                    pr::update_base(client, gh_repo, n, &item.base).await?;
                    println!("  PR #{n}: retargeted to base '{}'", item.base);
                }
                n
            }
            None => {
                let title = if item.summary.is_empty() {
                    item.bookmark.clone()
                } else {
                    item.summary.clone()
                };
                let n = pr::create_draft(
                    client,
                    gh_repo,
                    &title,
                    &item.bookmark,
                    &item.base,
                    "",
                )
                .await?;
                println!("  PR #{n}: created (draft) {} → {}", item.bookmark, item.base);
                n
            }
        };

        meta.pr_number = Some(number);
        meta.base_branch = Some(item.base.clone());
    }

    // 3. Maintain the nav comment on each PR.
    let rows: Vec<NavRow> = plan
        .items
        .iter()
        .map(|i| NavRow {
            pr_number: store.changes.get(&i.change_id).and_then(|m| m.pr_number),
            summary: i.summary.clone(),
        })
        .collect();
    for item in &plan.items {
        if let Some(num) = store.changes.get(&item.change_id).and_then(|m| m.pr_number) {
            let body = nav_comment::render(&plan.stack_name, &rows, num);
            let comment_id = nav_comment::upsert(client, gh_repo, num, &body).await?;
            if let Some(meta) = store.changes.get_mut(&item.change_id) {
                meta.nav_comment_id = Some(comment_id);
            }
        }
    }

    // Ensure the stack records its members.
    let entry = store.stacks.entry(plan.stack_name.clone()).or_default();
    for item in &plan.items {
        if !entry.members.contains(&item.change_id) {
            entry.members.push(item.change_id.clone());
        }
    }

    Ok(())
}

fn conflict_error(plan: &Plan) -> anyhow::Error {
    anyhow!(
        "{} change(s) skipped due to conflicts; resolve them in jj and re-run",
        plan.skipped.len()
    )
}

fn confirm() -> Result<bool> {
    print!("Proceed? [y/N] ");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim().to_lowercase().as_str(), "y" | "yes"))
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
