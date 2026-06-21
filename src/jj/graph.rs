//! Resolving change-ids to commits and ordering a set of changes by ancestry.
//!
//! The metadata store keys changes by stable change-id. To display or operate on
//! a stack we resolve each change-id to its current commit and order them from
//! trunk-nearest to head, using the index's ancestry relation.

use anyhow::{Context, Result, anyhow};
use jj_lib::backend::{ChangeId, CommitId};
use jj_lib::commit::Commit;
use jj_lib::object_id::ObjectId;
use jj_lib::repo::{ReadonlyRepo, Repo};

/// Outcome of resolving a stored change-id against the current repo.
pub enum Resolved {
    /// Exactly one visible commit for this change.
    Single(Commit),
    /// Multiple visible commits (divergent change); first is used, with a warning.
    Divergent(Commit),
    /// No visible commit (abandoned/hidden) — candidate for pruning.
    Missing,
}

/// Resolve a change-id (hex) to its current commit.
pub fn resolve_change(repo: &ReadonlyRepo, change_id_hex: &str) -> Result<Resolved> {
    let change_id = ChangeId::try_from_hex(change_id_hex)
        .filter(|c| !c.as_bytes().is_empty())
        .ok_or_else(|| anyhow!("invalid change-id in store: {change_id_hex:?}"))?;
    // `resolve_change_id` panics on an ambiguous (e.g. empty) prefix; full
    // change-ids are always unambiguous, and we guarded against empty above.
    let resolved = repo
        .resolve_change_id(&change_id)
        .context("failed to resolve change-id")?;
    let Some(targets) = resolved else {
        return Ok(Resolved::Missing);
    };
    let divergent = targets.is_divergent();
    let Some(commit_ids) = targets.into_visible() else {
        return Ok(Resolved::Missing);
    };
    let first = commit_ids
        .first()
        .ok_or_else(|| anyhow!("change resolved to empty target set"))?;
    let commit = repo
        .store()
        .get_commit(first)
        .context("failed to load commit")?;
    Ok(if divergent {
        Resolved::Divergent(commit)
    } else {
        Resolved::Single(commit)
    })
}

/// Order commits from trunk-nearest to head by ancestry.
///
/// For a linear stack, each commit's position is the number of other commits in
/// the set that are its ancestors. Small sets, so the O(n^2) ancestry checks are
/// fine.
pub fn order_by_ancestry(repo: &ReadonlyRepo, commits: &[Commit]) -> Result<Vec<Commit>> {
    let index = repo.index();
    let ids: Vec<&CommitId> = commits.iter().map(|c| c.id()).collect();

    let mut positioned: Vec<(usize, Commit)> = Vec::with_capacity(commits.len());
    for (i, commit) in commits.iter().enumerate() {
        let mut depth = 0usize;
        for (j, other) in ids.iter().enumerate() {
            if i == j {
                continue;
            }
            if index
                .is_ancestor(other, commit.id())
                .context("ancestry check failed")?
            {
                depth += 1;
            }
        }
        positioned.push((depth, commit.clone()));
    }
    positioned.sort_by_key(|(depth, _)| *depth);
    Ok(positioned.into_iter().map(|(_, c)| c).collect())
}

/// Safety cap on how far `chain_to_trunk` will walk, so a misconfigured trunk
/// can't sweep an entire repo history.
const MAX_CHAIN: usize = 1000;

/// Walk first-parent from `head` down to (but excluding) `trunk`, returning the
/// commits in trunk-nearest-first order. Used to discover a stack's members.
///
/// Stops at any commit that is an ancestor of (or equal to) `trunk` — not just
/// the trunk commit itself — so a history where trunk is reached only via a
/// merge's second parent doesn't cause the walk to run off into shared history.
pub fn chain_to_trunk(
    repo: &ReadonlyRepo,
    head: &CommitId,
    trunk: &CommitId,
) -> Result<Vec<Commit>> {
    let index = repo.index();
    let mut chain = Vec::new();
    let mut current = head.clone();
    let root = repo.store().root_commit_id().clone();
    loop {
        if current == root
            || &current == trunk
            || index
                .is_ancestor(&current, trunk)
                .context("ancestry check while walking to trunk")?
        {
            break;
        }
        if chain.len() >= MAX_CHAIN {
            eprintln!(
                "warning: stopped walking toward trunk after {MAX_CHAIN} commits; \
                 is '{}' the right trunk?",
                trunk.hex()
            );
            break;
        }
        let commit = repo
            .store()
            .get_commit(&current)
            .context("failed to load commit while walking to trunk")?;
        let next = commit.parent_ids().first().cloned();
        chain.push(commit);
        match next {
            Some(p) => current = p,
            None => break,
        }
    }
    chain.reverse();
    Ok(chain)
}

/// First line of a commit description, trimmed; empty string if none.
pub fn summary(commit: &Commit) -> String {
    commit
        .description()
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Short change-id (first 8 hex chars).
pub fn short_change_id(commit: &Commit) -> String {
    let hex = commit.change_id().hex();
    hex.chars().take(8).collect()
}
