//! Rendering and upserting the stack-navigation comment on each PR.
//!
//! Every managed PR carries one comment listing the whole stack, top to bottom,
//! marking the current PR. The comment is identified by a hidden HTML marker so
//! we can find and update it rather than posting duplicates.

use anyhow::{Context, Result};
use octocrab::Octocrab;

use crate::github::remote::GitHubRepo;

/// Hidden marker identifying a stacker-managed nav comment.
pub const MARKER: &str = "<!-- stacker:nav -->";

/// One row of the navigation table.
pub struct NavRow {
    pub pr_number: Option<u64>,
    pub summary: String,
}

/// Render the nav comment body for a stack, marking `current_pr`.
/// Rows are given trunk-first; we display head-first (top of stack on top).
pub fn render(stack_name: &str, rows_trunk_first: &[NavRow], current_pr: u64) -> String {
    let mut body = String::new();
    body.push_str(MARKER);
    body.push('\n');
    body.push_str(&format!("**Stack `{stack_name}`** (top → bottom)\n\n"));
    for row in rows_trunk_first.iter().rev() {
        let marker = if row.pr_number == Some(current_pr) {
            "👉"
        } else {
            "▸"
        };
        let label = match row.pr_number {
            Some(n) => format!("#{n}"),
            None => "(not submitted)".to_string(),
        };
        body.push_str(&format!("- {marker} {label} {}\n", row.summary));
    }
    body
}

/// Create or update the nav comment on `pr_number` to `body`.
pub async fn upsert(
    client: &Octocrab,
    repo: &GitHubRepo,
    pr_number: u64,
    body: &str,
) -> Result<u64> {
    let issues = client.issues(&repo.owner, &repo.repo);

    // Find an existing stacker nav comment.
    let mut page = issues
        .list_comments(pr_number)
        .per_page(100)
        .send()
        .await
        .with_context(|| format!("failed to list comments on PR #{pr_number}"))?;

    let mut existing_id = None;
    loop {
        for comment in &page.items {
            let has_marker = comment
                .body
                .as_deref()
                .map(|b| b.contains(MARKER))
                .unwrap_or(false);
            if has_marker {
                existing_id = Some(comment.id);
                break;
            }
        }
        if existing_id.is_some() {
            break;
        }
        match client.get_page(&page.next).await? {
            Some(next) => page = next,
            None => break,
        }
    }

    let comment = match existing_id {
        Some(id) => issues
            .update_comment(id, body)
            .await
            .with_context(|| format!("failed to update nav comment on PR #{pr_number}"))?,
        None => issues
            .create_comment(pr_number, body)
            .await
            .with_context(|| format!("failed to create nav comment on PR #{pr_number}"))?,
    };
    Ok(comment.id.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_head_first_with_current_marker() {
        let rows = vec![
            NavRow { pr_number: Some(42), summary: "add login form".into() },
            NavRow { pr_number: Some(43), summary: "add session store".into() },
            NavRow { pr_number: None, summary: "add db migration".into() },
        ];
        let body = render("feature", &rows, 43);
        assert!(body.starts_with(MARKER));
        // Head (db migration) appears before trunk-nearest (login form).
        let db = body.find("add db migration").unwrap();
        let login = body.find("add login form").unwrap();
        assert!(db < login);
        // Current PR (#43) is marked.
        assert!(body.contains("👉 #43 add session store"));
        assert!(body.contains("▸ #42 add login form"));
        assert!(body.contains("(not submitted) add db migration"));
    }
}
