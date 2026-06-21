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
    // A stack member is a PR *head* — a change carrying a bookmark. Commits
    // between consecutive bookmarks are the lower PR's content, not separate
    // members, so we adopt only the bookmarked changes. (This is what keeps a
    // big multi-commit branch from being registered as hundreds of "members".)
    let heads: Vec<_> = chain
        .iter()
        .filter_map(|c| bookmarks::for_commit(&loaded.repo, c.id()).map(|bk| (c, bk)))
        .collect();
    if heads.is_empty() {
        return Err(anyhow!(
            "no bookmarked changes between trunk ('{}') and @ to track.\n\
             stacker tracks PR heads — bookmark the changes you want as PRs first \
             (each bookmark becomes one PR).",
            config.trunk
        ));
    }

    // Best-effort GitHub adoption: discover each head's open PR if we can.
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

    println!("Tracking {} change(s) into stack '{name}':", heads.len());
    let mut members = Vec::with_capacity(heads.len());
    let mut adopted = 0usize;

    for (commit, bk) in &heads {
        let change_id = commit.change_id().hex();
        let short = &change_id[..8];

        let detail = match &gh {
            Some((client, repo)) => pr::find_detail_by_head(client, repo, bk).await?,
            None => None,
        };

        let mut meta = ChangeMeta {
            bookmark: Some(bk.clone()),
            ..Default::default()
        };
        match &detail {
            Some((number, base, state)) => {
                meta.pr_number = Some(*number);
                meta.base_branch = Some(base.clone());
                adopted += 1;
                println!("  {short}  {bk} → PR #{number} [{}] (base {base})", state.label());
            }
            None => println!("  {short}  {bk}"),
        }

        store.changes.insert(change_id.clone(), meta);
        members.push(change_id);
    }

    store.stacks.insert(name.clone(), Stack { members });
    store.save()?;

    let n = heads.len();
    if gh.is_some() {
        println!("\nTracked stack '{name}': {n} change(s), {adopted} with existing PR(s).");
    } else {
        println!("\nTracked stack '{name}': {n} change(s).");
    }
    println!("Run `stacker status` to review, or `stacker submit {name}`.");
    Ok(())
}
