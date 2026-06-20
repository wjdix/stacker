//! Reading pull-request state from GitHub.

use anyhow::{Context, Result};
use octocrab::Octocrab;
use octocrab::models::IssueState;
use octocrab::models::pulls::PullRequest;

use crate::github::remote::GitHubRepo;

/// Coarse PR lifecycle state for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Draft,
    Closed,
    Merged,
}

impl PrState {
    pub fn label(self) -> &'static str {
        match self {
            PrState::Open => "open",
            PrState::Draft => "draft",
            PrState::Closed => "closed",
            PrState::Merged => "merged",
        }
    }
}

fn classify(pr: &PullRequest) -> PrState {
    if pr.merged.unwrap_or(false) {
        return PrState::Merged;
    }
    match pr.state {
        Some(IssueState::Closed) => PrState::Closed,
        _ => {
            if pr.draft.unwrap_or(false) {
                PrState::Draft
            } else {
                PrState::Open
            }
        }
    }
}

/// Fetch the coarse state of a PR by number.
pub async fn fetch_state(
    client: &Octocrab,
    repo: &GitHubRepo,
    number: u64,
) -> Result<PrState> {
    let pr = client
        .pulls(&repo.owner, &repo.repo)
        .get(number)
        .await
        .with_context(|| format!("failed to fetch PR #{number}"))?;
    Ok(classify(&pr))
}

/// Find an existing PR whose head branch is `head` (any state). Returns its
/// number if present.
pub async fn find_by_head(
    client: &Octocrab,
    repo: &GitHubRepo,
    head: &str,
) -> Result<Option<u64>> {
    let qualified = format!("{}:{}", repo.owner, head);
    let page = client
        .pulls(&repo.owner, &repo.repo)
        .list()
        .state(octocrab::params::State::All)
        .head(qualified)
        .per_page(10u8)
        .send()
        .await
        .with_context(|| format!("failed to list PRs for head '{head}'"))?;
    Ok(page.items.into_iter().map(|pr| pr.number).next())
}

/// Find an existing PR by head branch, returning its number, current base
/// branch, and coarse state. Used by `import` to adopt PRs read-only.
pub async fn find_detail_by_head(
    client: &Octocrab,
    repo: &GitHubRepo,
    head: &str,
) -> Result<Option<(u64, String, PrState)>> {
    let qualified = format!("{}:{}", repo.owner, head);
    let page = client
        .pulls(&repo.owner, &repo.repo)
        .list()
        .state(octocrab::params::State::All)
        .head(qualified)
        .per_page(10u8)
        .send()
        .await
        .with_context(|| format!("failed to list PRs for head '{head}'"))?;
    Ok(page
        .items
        .into_iter()
        .next()
        .map(|pr| (pr.number, pr.base.ref_field.clone(), classify(&pr))))
}

/// Create a draft PR. `head`/`base` are branch names; returns the new number.
pub async fn create_draft(
    client: &Octocrab,
    repo: &GitHubRepo,
    title: &str,
    head: &str,
    base: &str,
    body: &str,
) -> Result<u64> {
    let pr = client
        .pulls(&repo.owner, &repo.repo)
        .create(title, head, base)
        .body(body.to_string())
        .draft(true)
        .send()
        .await
        .with_context(|| format!("failed to create PR for '{head}' -> '{base}'"))?;
    Ok(pr.number)
}

/// Retarget an existing PR's base branch.
pub async fn update_base(
    client: &Octocrab,
    repo: &GitHubRepo,
    number: u64,
    base: &str,
) -> Result<()> {
    client
        .pulls(&repo.owner, &repo.repo)
        .update(number)
        .base(base.to_string())
        .send()
        .await
        .with_context(|| format!("failed to retarget PR #{number} to base '{base}'"))?;
    Ok(())
}
