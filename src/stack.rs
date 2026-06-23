//! The stack model: combine stored metadata with the live jj graph.
//!
//! A named stack's membership is stored (by change-id); the *order* is derived
//! here from the commit graph. Each entry is joined with its stored per-change
//! metadata (bookmark, PR number, base).

use std::collections::HashSet;

use anyhow::{Context, Result, anyhow};
use jj_lib::backend::CommitId;
use jj_lib::object_id::ObjectId;
use jj_lib::repo::{ReadonlyRepo, Repo};

use octocrab::Octocrab;

use crate::github::pr;
use crate::github::remote::GitHubRepo;
use crate::jj::graph::{self, Resolved};
use crate::jj::repo::LoadedRepo;
use crate::store::Store;

/// One change in a resolved stack.
pub struct StackEntry {
    /// Change-id hex (the store key).
    pub change_id: String,
    pub short_change_id: String,
    pub commit_id: jj_lib::backend::CommitId,
    pub summary: String,
    pub bookmark: Option<String>,
    pub pr_number: Option<u64>,
    /// Bookmark this entry should be based on (entry below, or trunk for bottom).
    pub expected_base: String,
    pub has_conflict: bool,
    pub divergent: bool,
}

/// A stack resolved against the current repo, ordered trunk-nearest first.
pub struct ResolvedStack {
    pub name: String,
    pub entries: Vec<StackEntry>,
    /// Member change-ids that no longer resolve (abandoned/hidden) — prunable.
    pub missing: Vec<String>,
    /// Members that have landed (are now ancestors of trunk) — prunable.
    pub landed: Vec<String>,
}

/// Discover changes appended to the stack by another tool: any **bookmarked**
/// change that descends from the stack's current head and isn't already a
/// member is added (a PR head = one stack member). Returns how many were added.
pub fn discover_appended_members(
    repo: &ReadonlyRepo,
    resolved: &ResolvedStack,
    store: &mut Store,
    name: &str,
) -> Result<usize> {
    let Some(head) = resolved.entries.last() else {
        return Ok(0); // empty stack — nothing to extend from
    };
    let head_id = head.commit_id.clone();
    let index = repo.index();

    let mut members: HashSet<String> = store
        .stacks
        .get(name)
        .map(|s| s.members.iter().cloned().collect())
        .unwrap_or_default();

    let mut added = Vec::new();
    for (_bookmark, target) in repo.view().local_bookmarks() {
        let Some(commit_id) = target.as_normal() else {
            continue;
        };
        if commit_id == &head_id {
            continue;
        }
        // Strict descendant of the head?
        if index
            .is_ancestor(&head_id, commit_id)
            .context("ancestry check while discovering appended members")?
        {
            let change_id = repo
                .store()
                .get_commit(commit_id)
                .context("load bookmarked commit")?
                .change_id()
                .hex();
            if members.insert(change_id.clone()) {
                added.push(change_id);
            }
        }
    }

    if !added.is_empty()
        && let Some(stack) = store.stacks.get_mut(name)
    {
        stack.members.extend(added.iter().cloned());
    }
    Ok(added.len())
}

/// Reconcile a stack with reality and resolve it: discover appended bookmarked
/// changes, adopt externally-opened PRs, then resolve. Returns the resolved
/// stack and whether the stored metadata changed (so the caller can persist).
pub async fn refresh_and_resolve(
    loaded: &LoadedRepo,
    store: &mut Store,
    name: &str,
    trunk_bookmark: &str,
    trunk_id: Option<&CommitId>,
    gh: Option<(&Octocrab, &GitHubRepo)>,
) -> Result<(ResolvedStack, bool)> {
    let mut changed = false;
    let mut resolved = resolve(&loaded.repo, store, name, trunk_bookmark, trunk_id)?;

    if discover_appended_members(&loaded.repo, &resolved, store, name)? > 0 {
        changed = true;
        resolved = resolve(&loaded.repo, store, name, trunk_bookmark, trunk_id)?;
    }

    if let Some((client, repo)) = gh
        && adopt_external_prs(&resolved, store, client, repo).await? > 0
    {
        changed = true;
        resolved = resolve(&loaded.repo, store, name, trunk_bookmark, trunk_id)?;
    }

    Ok((resolved, changed))
}

/// Adopt externally-opened PRs: for each resolved entry that has a bookmark but
/// no recorded PR, look it up on GitHub by head branch and record its number and
/// base. Returns how many were newly adopted; the caller persists the store.
pub async fn adopt_external_prs(
    resolved: &ResolvedStack,
    store: &mut Store,
    client: &Octocrab,
    repo: &GitHubRepo,
) -> Result<usize> {
    let mut adopted = 0;
    for entry in &resolved.entries {
        if entry.pr_number.is_some() {
            continue;
        }
        let Some(bookmark) = &entry.bookmark else {
            continue;
        };
        if let Some((number, base, _state)) = pr::find_detail_by_head(client, repo, bookmark).await?
        {
            let meta = store.changes.entry(entry.change_id.clone()).or_default();
            meta.bookmark = Some(bookmark.clone());
            meta.pr_number = Some(number);
            meta.base_branch = Some(base);
            adopted += 1;
        }
    }
    Ok(adopted)
}

/// Infer which tracked stack the current change (@) belongs to.
///
/// A stack matches if any of its members is @ or an ancestor of @. The stack
/// with the most matching members wins; ambiguity or no match is an error
/// directing the user to name a stack explicitly.
pub fn infer_stack_name(loaded: &LoadedRepo, store: &Store) -> Result<String> {
    let repo = &loaded.repo;
    let wc_name = loaded.workspace.workspace_name();
    let at = repo
        .view()
        .get_wc_commit_id(wc_name)
        .ok_or_else(|| anyhow!("no working-copy commit for this workspace"))?
        .clone();
    let index = repo.index();

    let mut best: Option<(usize, String)> = None;
    let mut tie = false;
    for (name, stack) in &store.stacks {
        let mut count = 0usize;
        for change_hex in &stack.members {
            let commit = match graph::resolve_change(repo, change_hex)? {
                Resolved::Single(c) | Resolved::Divergent(c) => c,
                Resolved::Missing => continue,
            };
            if commit.id() == &at || index.is_ancestor(commit.id(), &at)? {
                count += 1;
            }
        }
        if count == 0 {
            continue;
        }
        match &best {
            Some((bc, _)) if *bc > count => {}
            Some((bc, _)) if *bc == count => tie = true,
            _ => {
                best = Some((count, name.clone()));
                tie = false;
            }
        }
    }

    match best {
        Some((_, name)) if !tie => Ok(name),
        Some(_) => Err(anyhow!(
            "multiple tracked stacks contain the current change (@); specify one explicitly"
        )),
        None => Err(anyhow!(
            "no tracked stack contains the current change (@); specify a stack name"
        )),
    }
}

/// Resolve a named stack: order its members and join stored metadata.
///
/// `trunk_bookmark` is the base shown for the bottom entry. When `trunk_id` is
/// given, members that have **landed** (are now ancestors of trunk) are skipped
/// and reported in `landed` — this self-heals stacks whose lower changes have
/// merged, and keeps the (O(n²)) ordering bounded to live members.
pub fn resolve(
    repo: &ReadonlyRepo,
    store: &Store,
    name: &str,
    trunk_bookmark: &str,
    trunk_id: Option<&CommitId>,
) -> Result<ResolvedStack> {
    let stack = store
        .stacks
        .get(name)
        .ok_or_else(|| anyhow!("no such stack: {name}"))?;

    let mut commits = Vec::new();
    let mut divergent_changes = HashSet::new();
    let mut missing = Vec::new();
    let mut landed = Vec::new();

    for change_hex in &stack.members {
        let (commit, divergent) = match graph::resolve_change(repo, change_hex)? {
            Resolved::Single(c) => (c, false),
            Resolved::Divergent(c) => (c, true),
            Resolved::Missing => {
                missing.push(change_hex.clone());
                continue;
            }
        };
        if let Some(trunk) = trunk_id
            && repo
                .index()
                .is_ancestor(commit.id(), trunk)
                .context("ancestry check while resolving stack")?
        {
            landed.push(change_hex.clone());
            continue;
        }
        if divergent {
            divergent_changes.insert(commit.change_id().hex());
        }
        commits.push(commit);
    }

    let ordered = graph::order_by_ancestry(repo, &commits)?;

    let mut entries = Vec::with_capacity(ordered.len());
    let mut prev_bookmark: Option<String> = None;
    for commit in &ordered {
        let change_id = commit.change_id().hex();
        let meta = store.changes.get(&change_id);
        // Prefer the stored bookmark; otherwise adopt a jj bookmark at this commit.
        let bookmark = meta
            .and_then(|m| m.bookmark.clone())
            .or_else(|| crate::jj::bookmarks::for_commit(repo, commit.id()));
        let expected_base = prev_bookmark
            .clone()
            .unwrap_or_else(|| trunk_bookmark.to_string());

        entries.push(StackEntry {
            short_change_id: graph::short_change_id(commit),
            commit_id: commit.id().clone(),
            summary: graph::summary(commit),
            bookmark: bookmark.clone(),
            pr_number: meta.and_then(|m| m.pr_number),
            expected_base,
            has_conflict: commit.has_conflict(),
            divergent: divergent_changes.contains(&change_id),
            change_id,
        });
        prev_bookmark = bookmark;
    }

    Ok(ResolvedStack {
        name: name.to_string(),
        entries,
        missing,
        landed,
    })
}
