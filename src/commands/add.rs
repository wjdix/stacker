//! `stacker add [stack] [-m msg]` — add a new change on top of an existing stack.

use anyhow::{Result, anyhow};
use jj_lib::object_id::ObjectId;

use crate::config::Config;
use crate::jj::{repo, workspace_ops};
use crate::paths;
use crate::stack;
use crate::store::Store;

pub async fn run(stack_name: Option<String>, message: Option<String>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    crate::jj::snapshot::snapshot_working_copy(&cwd);
    let mut loaded = repo::load(&cwd).await?;
    let state_dir = paths::repo_state_dir(loaded.repo_path())?;
    let config = Config::load(&state_dir)?;
    let mut store = Store::load(&state_dir)?;

    let name = match stack_name {
        Some(n) => n,
        None => stack::infer_stack_name(&loaded, &store)?,
    };

    let resolved = stack::resolve(&loaded.repo, &store, &name, &config.trunk)?;
    let head = resolved
        .entries
        .last()
        .map(|e| e.commit_id.clone())
        .ok_or_else(|| anyhow!("stack '{name}' has no changes to build on"))?;

    let new_commit =
        workspace_ops::new_on_top(&mut loaded.workspace, &loaded.repo, &head, message.as_deref())
            .await?;
    let change_id = new_commit.change_id().hex();

    if let Some(stack) = store.stacks.get_mut(&name)
        && !stack.members.contains(&change_id) {
            stack.members.push(change_id.clone());
        }
    store.save()?;

    let short: String = change_id.chars().take(8).collect();
    println!("Added new change {short} on top of stack '{name}'. Make your edits, then `stacker submit {name}`.");
    Ok(())
}
