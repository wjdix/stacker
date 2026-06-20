//! Bookmark naming and mutation.
//!
//! Stacker owns bookmark creation. Names are `<prefix><slug>` where the slug is
//! derived from the change's first description line, with a numeric suffix on
//! collision.

use std::collections::HashSet;

use jj_lib::backend::CommitId;
use jj_lib::repo::ReadonlyRepo;

const MAX_SLUG_LEN: usize = 50;

/// Convert arbitrary text into a lowercase, dash-separated slug.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(c.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    if out.len() > MAX_SLUG_LEN {
        out.truncate(MAX_SLUG_LEN);
        while out.ends_with('-') {
            out.pop();
        }
    }
    out
}

/// Generate a unique bookmark name from a prefix and a change summary, avoiding
/// names already present in `existing`.
pub fn generate_name(prefix: &str, summary: &str, existing: &HashSet<String>) -> String {
    let slug = {
        let s = slugify(summary);
        if s.is_empty() {
            "change".to_string()
        } else {
            s
        }
    };
    let base = format!("{prefix}{slug}");
    if !existing.contains(&base) {
        return base;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if !existing.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// First local bookmark pointing at `commit_id`, if any. Used to adopt a
/// bookmark the user created manually before stacker tracked it.
pub fn for_commit(repo: &ReadonlyRepo, commit_id: &CommitId) -> Option<String> {
    repo.view()
        .local_bookmarks_for_commit(commit_id)
        .map(|(name, _)| name.as_str().to_string())
        .next()
}

/// All local bookmark names currently in the repo.
pub fn existing_names(repo: &ReadonlyRepo) -> HashSet<String> {
    repo.view()
        .local_bookmarks()
        .map(|(name, _)| name.as_str().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugifies_descriptions() {
        assert_eq!(slugify("Add login form"), "add-login-form");
        assert_eq!(slugify("  Fix:  the THING!! "), "fix-the-thing");
        assert_eq!(slugify("emoji 🚀 rocket"), "emoji-rocket");
        assert_eq!(slugify("---"), "");
    }

    #[test]
    fn generates_unique_names() {
        let mut existing = HashSet::new();
        let a = generate_name("wd/", "Add login form", &existing);
        assert_eq!(a, "wd/add-login-form");
        existing.insert(a);
        let b = generate_name("wd/", "Add login form", &existing);
        assert_eq!(b, "wd/add-login-form-2");
        existing.insert(b);
        let c = generate_name("wd/", "Add login form", &existing);
        assert_eq!(c, "wd/add-login-form-3");
    }

    #[test]
    fn empty_summary_falls_back() {
        let existing = HashSet::new();
        assert_eq!(generate_name("", "", &existing), "change");
        assert_eq!(generate_name("u/", "", &existing), "u/change");
    }
}
