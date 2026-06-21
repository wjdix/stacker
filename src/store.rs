//! Global, per-repo metadata store.
//!
//! Stacks group changes by name; the order of changes within a stack is *not*
//! stored here — it is derived from the jj commit graph at runtime. Per-change
//! metadata (bookmark, PR number, last-applied base, nav-comment id) is keyed by
//! the stable jj change-id (hex), so it survives rebases that rewrite commit ids.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// A named stack: a set of member changes (by change-id hex). Order is derived
/// from the graph, not persisted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stack {
    pub members: Vec<String>,
}

/// Per-change metadata, keyed by change-id hex in [`Store::changes`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChangeMeta {
    /// Current bookmark name (== PR head branch).
    pub bookmark: Option<String>,
    /// GitHub PR number, once submitted.
    pub pr_number: Option<u64>,
    /// Base branch last applied to the PR on GitHub.
    pub base_branch: Option<String>,
    /// Id of the auto-maintained stack-navigation comment.
    pub nav_comment_id: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Store {
    pub stacks: BTreeMap<String, Stack>,
    pub changes: BTreeMap<String, ChangeMeta>,
    #[serde(skip)]
    path: PathBuf,
}

impl Store {
    const FILE_NAME: &'static str = "stacks.json";

    /// Load the store from `state_dir/stacks.json`, or an empty store if absent.
    pub fn load(state_dir: &Path) -> Result<Self> {
        let path = state_dir.join(Self::FILE_NAME);
        let mut store = if path.exists() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            serde_json::from_str::<Store>(&text)
                .with_context(|| format!("failed to parse {}", path.display()))?
        } else {
            Store::default()
        };
        store.path = path;
        Ok(store)
    }

    /// Drop a change from a stack's membership and forget its metadata.
    /// Returns true if the change was a member.
    pub fn drop_change(&mut self, stack: &str, change_id: &str) -> bool {
        let removed = if let Some(s) = self.stacks.get_mut(stack) {
            let before = s.members.len();
            s.members.retain(|m| m != change_id);
            s.members.len() != before
        } else {
            false
        };
        self.changes.remove(change_id);
        removed
    }

    /// Stop tracking a stack: remove it and forget metadata for any of its
    /// changes not referenced by another stack. Returns the number of member
    /// changes the stack had, or `None` if the stack didn't exist. Does not
    /// touch jj or GitHub.
    pub fn untrack_stack(&mut self, name: &str) -> Option<usize> {
        let stack = self.stacks.remove(name)?;
        for change_id in &stack.members {
            let still_referenced = self.stacks.values().any(|s| s.members.contains(change_id));
            if !still_referenced {
                self.changes.remove(change_id);
            }
        }
        Some(stack.members.len())
    }

    /// Remove the given change-ids from a stack's membership (e.g. members that
    /// have landed or no longer resolve), forgetting metadata no longer
    /// referenced by any stack. Returns the number removed.
    pub fn prune_stack_members(&mut self, stack: &str, stale: &[String]) -> usize {
        if stale.is_empty() {
            return 0;
        }
        let stale: std::collections::HashSet<&String> = stale.iter().collect();
        let removed = if let Some(s) = self.stacks.get_mut(stack) {
            let before = s.members.len();
            s.members.retain(|m| !stale.contains(m));
            before - s.members.len()
        } else {
            0
        };
        for change_id in stale {
            let still_referenced = self.stacks.values().any(|s| s.members.contains(change_id));
            if !still_referenced {
                self.changes.remove(change_id);
            }
        }
        removed
    }

    /// Persist the store atomically (write to a temp file, then rename).
    pub fn save(&self) -> Result<()> {
        let text = serde_json::to_string_pretty(self).context("failed to serialize store")?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &text)
            .with_context(|| format!("failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("failed to replace {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_change_removes_member_and_meta() {
        let mut store = Store::default();
        store.stacks.insert(
            "feature".into(),
            Stack { members: vec!["a".into(), "b".into(), "c".into()] },
        );
        store.changes.insert("a".into(), ChangeMeta::default());

        assert!(store.drop_change("feature", "a"));
        assert_eq!(store.stacks["feature"].members, vec!["b", "c"]);
        assert!(!store.changes.contains_key("a"));
        // Dropping a non-member returns false.
        assert!(!store.drop_change("feature", "zzz"));
    }

    #[test]
    fn untrack_stack_prunes_orphans_but_keeps_shared() {
        let mut store = Store::default();
        store
            .stacks
            .insert("a".into(), Stack { members: vec!["x".into(), "y".into()] });
        store
            .stacks
            .insert("b".into(), Stack { members: vec!["y".into(), "z".into()] });
        store.changes.insert("x".into(), ChangeMeta::default());
        store.changes.insert("y".into(), ChangeMeta::default());
        store.changes.insert("z".into(), ChangeMeta::default());

        // Untracking 'a' (2 members) forgets x (orphan) but keeps y (still in 'b').
        assert_eq!(store.untrack_stack("a"), Some(2));
        assert!(!store.stacks.contains_key("a"));
        assert!(!store.changes.contains_key("x"));
        assert!(store.changes.contains_key("y"));
        assert!(store.changes.contains_key("z"));

        // Untracking a missing stack returns None.
        assert_eq!(store.untrack_stack("missing"), None);
    }

    #[test]
    fn prune_stack_members_drops_stale_and_orphan_meta() {
        let mut store = Store::default();
        store.stacks.insert(
            "feature".into(),
            Stack { members: vec!["a".into(), "b".into(), "c".into()] },
        );
        store.stacks.insert("other".into(), Stack { members: vec!["c".into()] });
        for id in ["a", "b", "c"] {
            store.changes.insert(id.into(), ChangeMeta::default());
        }

        // Prune a (orphan → meta dropped) and c (still in 'other' → meta kept).
        let removed = store.prune_stack_members("feature", &["a".into(), "c".into()]);
        assert_eq!(removed, 2);
        assert_eq!(store.stacks["feature"].members, vec!["b"]);
        assert!(!store.changes.contains_key("a")); // orphaned → forgotten
        assert!(store.changes.contains_key("b"));
        assert!(store.changes.contains_key("c")); // still referenced by 'other'
    }

    #[test]
    fn store_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("stacker-store-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let mut store = Store::load(&dir).unwrap();
        store
            .stacks
            .insert("feature".to_string(), Stack { members: vec!["abc123".to_string()] });
        store.changes.insert(
            "abc123".to_string(),
            ChangeMeta {
                bookmark: Some("wd/feature".to_string()),
                pr_number: Some(42),
                base_branch: Some("main".to_string()),
                nav_comment_id: None,
            },
        );
        store.save().unwrap();

        let reloaded = Store::load(&dir).unwrap();
        assert_eq!(reloaded.stacks["feature"].members, vec!["abc123"]);
        assert_eq!(reloaded.changes["abc123"].pr_number, Some(42));
        assert_eq!(
            reloaded.changes["abc123"].bookmark.as_deref(),
            Some("wd/feature")
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
