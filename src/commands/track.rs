//! `stacker track <name>` — adopt the current trunk→@ chain as a named stack.
//!
//! Registers the chain of described changes from trunk to @. When GitHub is
//! reachable, it also matches each change's bookmark to its open PR and records
//! the PR number and current base (this folds in the former `import` command).

use anyhow::{Result, anyhow};
use jj_lib::object_id::ObjectId;

use crate::config::Config;
use crate::github::client;
use crate::github::pr;
use crate::github::remote;
use crate::jj::{bookmarks, graph, repo, trunk};
use crate::paths;
use crate::store::{ChangeMeta, Stack, Store};

pub async fn run(name: String) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let loaded = repo::load(&cwd).await?;
    let state_dir = paths::repo_state_dir(loaded.repo_path())?;
    let config = Config::load(&state_dir)?;
    let mut store = Store::load(&state_dir)?;

    if store.stacks.contains_key(&name) {
        return Err(anyhow!("stack '{name}' already exists"));
    }

    let trunk_id = trunk::resolve(&loaded.repo, &config.trunk, &config.remote)?;
    let wc_name = loaded.workspace.workspace_name();
    let at = loaded
        .repo
        .view()
        .get_wc_commit_id(wc_name)
        .ok_or_else(|| anyhow!("no working-copy commit for this workspace"))?
        .clone();

    let chain = graph::chain_to_trunk(&loaded.repo, &at, &trunk_id)?;
    let described: Vec<_> = chain
        .iter()
        .filter(|c| !c.description().trim().is_empty())
        .collect();
    if described.is_empty() {
        return Err(anyhow!(
            "no described changes between trunk ('{}') and @ to track",
            config.trunk
        ));
    }

    // Best-effort GitHub adoption: discover each change's open PR if we can.
    let gh = match (
        remote::resolve(&loaded.repo, &config.remote).ok(),
        client::build().ok(),
    ) {
        (Some(r), Some(c)) => Some((c, r)),
        _ => None,
    };
    if gh.is_none() {
        eprintln!("note: GitHub unavailable; tracking changes without PR discovery.");
    }

    println!("Tracking {} change(s) into stack '{name}':", described.len());
    let mut members = Vec::with_capacity(described.len());
    let mut adopted = 0usize;

    for commit in &described {
        let change_id = commit.change_id().hex();
        let short = &change_id[..8];
        let bookmark = bookmarks::for_commit(&loaded.repo, commit.id());

        let detail = match (&gh, &bookmark) {
            (Some((client, repo)), Some(bk)) => pr::find_detail_by_head(client, repo, bk).await?,
            _ => None,
        };

        let mut meta = ChangeMeta {
            bookmark: bookmark.clone(),
            ..Default::default()
        };
        match (&bookmark, &detail) {
            (_, Some((number, base, state))) => {
                meta.pr_number = Some(*number);
                meta.base_branch = Some(base.clone());
                adopted += 1;
                let bk = bookmark.as_deref().unwrap_or("?");
                println!("  {short}  {bk} → PR #{number} [{}] (base {base})", state.label());
            }
            (Some(bk), None) => println!("  {short}  {bk}"),
            (None, None) => println!("  {short}  (no bookmark)"),
        }

        store.changes.insert(change_id.clone(), meta);
        members.push(change_id);
    }

    store.stacks.insert(name.clone(), Stack { members });
    store.save()?;

    let n = described.len();
    if gh.is_some() {
        println!("\nTracked stack '{name}': {n} change(s), {adopted} with existing PR(s).");
    } else {
        println!("\nTracked stack '{name}': {n} change(s).");
    }
    println!("Run `stacker status` to review, or `stacker submit {name}`.");
    Ok(())
}
