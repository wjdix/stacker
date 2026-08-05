//! `stacker insert [stack] [--before <change>] [-m msg]` — insert a new change
//! at the bottom of an existing stack, or directly below a given member.

use anyhow::{Result, anyhow, bail};
use jj_lib::object_id::ObjectId;

use crate::config::Config;
use crate::jj::{repo, trunk, workspace_ops};
use crate::paths;
use crate::stack;
use crate::store::Store;

pub async fn run(
    stack_name: Option<String>,
    before: Option<String>,
    message: Option<String>,
) -> Result<()> {
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

    let trunk_id = trunk::resolve(&loaded.repo, &config.trunk, &config.remote).ok();
    let resolved = stack::resolve(&loaded.repo, &store, &name, &config.trunk, trunk_id.as_ref())?;
    if resolved.entries.is_empty() {
        bail!("stack '{name}' has no changes to insert below");
    }

    // The new change goes directly below this entry (the bottom by default);
    // its parent is that entry's own parent, so nothing else is rebased around.
    let member_ids: Vec<&str> = resolved
        .entries
        .iter()
        .map(|e| e.change_id.as_str())
        .collect();
    let index = match &before {
        None => 0,
        Some(prefix) => find_member(&member_ids, prefix)?,
    };
    let child = resolved.entries[index].commit_id.clone();
    let below = resolved.entries[index].short_change_id.clone();

    let new_commit =
        workspace_ops::insert_before(&mut loaded.workspace, &loaded.repo, &child, message.as_deref())
            .await?;
    let change_id = new_commit.change_id().hex();

    if let Some(stack) = store.stacks.get_mut(&name)
        && !stack.members.contains(&change_id) {
            stack.members.push(change_id.clone());
        }
    store.save()?;

    let short: String = change_id.chars().take(8).collect();
    let position = if index == 0 {
        format!("at the bottom of stack '{name}'")
    } else {
        format!("below {below} in stack '{name}'")
    };
    println!(
        "Inserted new change {short} {position}. Make your edits, then `stacker submit {name}` to update PR bases."
    );
    Ok(())
}

/// Find the single stack member whose change-id starts with `prefix`
/// (case-insensitive). Errors on no match or an ambiguous prefix.
fn find_member(member_change_ids: &[&str], prefix: &str) -> Result<usize> {
    let prefix = prefix.trim().to_lowercase();
    if prefix.is_empty() {
        bail!("--before requires a change-id prefix");
    }
    let matches: Vec<usize> = member_change_ids
        .iter()
        .enumerate()
        .filter(|(_, id)| id.starts_with(&prefix))
        .map(|(i, _)| i)
        .collect();
    match matches.as_slice() {
        [i] => Ok(*i),
        [] => Err(anyhow!("no member of this stack matches change-id prefix '{prefix}'")),
        _ => Err(anyhow!("change-id prefix '{prefix}' matches multiple stack members")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_member_matches_unique_prefix() {
        let ids = ["aabb1122", "ccdd3344", "ccff5566"];
        assert_eq!(find_member(&ids, "aa").unwrap(), 0);
        assert_eq!(find_member(&ids, "ccdd").unwrap(), 1);
        // Case-insensitive (the TUI shows lowercase hex, but be forgiving).
        assert_eq!(find_member(&ids, "CCFF").unwrap(), 2);
    }

    #[test]
    fn find_member_rejects_missing_ambiguous_and_empty() {
        let ids = ["aabb1122", "ccdd3344", "ccff5566"];
        assert!(find_member(&ids, "zz").is_err());
        assert!(find_member(&ids, "cc").is_err()); // ambiguous
        assert!(find_member(&ids, "  ").is_err());
    }
}
