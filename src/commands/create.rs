//! `stacker create <name>` — register the current trunk→@ chain as a named stack.

use anyhow::{Result, anyhow};
use jj_lib::object_id::ObjectId;

use crate::config::Config;
use crate::jj::{graph, repo, trunk};
use crate::paths;
use crate::store::{Stack, Store};

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
    // Skip commits with no description (e.g. the empty working-copy commit).
    let members: Vec<String> = chain
        .iter()
        .filter(|c| !c.description().trim().is_empty())
        .map(|c| c.change_id().hex())
        .collect();

    if members.is_empty() {
        return Err(anyhow!(
            "no described changes between trunk ('{}') and @ to form a stack",
            config.trunk
        ));
    }

    let count = members.len();
    store.stacks.insert(name.clone(), Stack { members });
    store.save()?;
    println!("Created stack '{name}' with {count} change(s). Run `stacker submit {name}` to open PRs.");
    Ok(())
}
