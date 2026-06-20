//! `stacker import <name>` — adopt an existing stack (changes + open PRs) under
//! stacker's control, read-only.
//!
//! Like `create`, it registers the current trunk→@ chain as a named stack. In
//! addition, for each change it matches the change's bookmark to an existing
//! GitHub PR and records the PR number and its current base branch. Nothing is
//! pushed, created, or retargeted — a later `submit`/`sync` does that.

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
            "no described changes between trunk ('{}') and @ to import",
            config.trunk
        ));
    }

    let gh_repo = remote::resolve(&loaded.repo, &config.remote)?;
    let github = client::build()?;

    println!(
        "Importing {} change(s) into stack '{name}' from {}/{}:",
        described.len(),
        gh_repo.owner,
        gh_repo.repo
    );

    let mut members = Vec::with_capacity(described.len());
    let mut adopted = 0usize;
    let mut without = 0usize;

    for commit in &described {
        let change_id = commit.change_id().hex();
        let short = &change_id[..8];
        let bookmark = bookmarks::for_commit(&loaded.repo, commit.id());

        let mut meta = ChangeMeta {
            bookmark: bookmark.clone(),
            ..Default::default()
        };

        match &bookmark {
            Some(bk) => match pr::find_detail_by_head(&github, &gh_repo, bk).await? {
                Some((number, base, state)) => {
                    meta.pr_number = Some(number);
                    meta.base_branch = Some(base.clone());
                    adopted += 1;
                    println!("  {short}  {bk} → PR #{number} [{}] (base {base})", state.label());
                }
                None => {
                    without += 1;
                    println!("  {short}  {bk} → no open PR found");
                }
            },
            None => {
                without += 1;
                println!("  {short}  (no bookmark) → cannot match a PR");
            }
        }

        store.changes.insert(change_id.clone(), meta);
        members.push(change_id);
    }

    store.stacks.insert(name.clone(), Stack { members });
    store.save()?;

    println!(
        "\nImported stack '{name}': {} change(s), {adopted} with existing PR(s){}.",
        described.len(),
        if without > 0 {
            format!(", {without} without")
        } else {
            String::new()
        }
    );
    println!("Run `stacker status` to review, or `stacker submit {name}` to reconcile.");
    Ok(())
}
