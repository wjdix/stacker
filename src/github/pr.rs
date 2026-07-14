//! Reading pull-request state from GitHub.

use anyhow::{Context, Result};
use octocrab::Octocrab;
use octocrab::models::IssueState;
use octocrab::models::pulls::{MergeableState, PullRequest};
use octocrab::params::pulls::MergeMethod;
use octocrab::params::repos::Commitish;
use serde_json::json;

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

/// Fields needed to safely merge a pull request and expose it to a readiness command.
#[derive(Debug, Clone)]
pub struct MergeCandidate {
    pub state: PrState,
    pub head_sha: String,
    pub url: String,
}

/// Fetch the current state and head SHA of a merge candidate.
pub async fn fetch_merge_candidate(
    client: &Octocrab,
    repo: &GitHubRepo,
    number: u64,
) -> Result<MergeCandidate> {
    let pr = client
        .pulls(&repo.owner, &repo.repo)
        .get(number)
        .await
        .with_context(|| format!("failed to fetch PR #{number}"))?;
    Ok(MergeCandidate {
        state: classify(&pr),
        head_sha: pr.head.sha.clone(),
        url: pr
            .html_url
            .as_ref()
            .map(|u| u.to_string())
            .unwrap_or_default(),
    })
}

/// Merge a PR through GitHub's REST API, guarded by its previously observed
/// head SHA. A successful HTTP response can still report `merged: false`.
pub async fn merge(
    client: &Octocrab,
    repo: &GitHubRepo,
    number: u64,
    head_sha: &str,
    method: MergeMethod,
) -> Result<()> {
    let result = client
        .pulls(&repo.owner, &repo.repo)
        .merge(number)
        .sha(head_sha)
        .method(method)
        .send()
        .await
        .with_context(|| format!("failed to merge PR #{number}"))?;
    if !result.merged {
        return Err(anyhow::anyhow!(
            "GitHub did not merge PR #{number}: {}",
            result.message.as_deref().unwrap_or("no reason provided")
        ));
    }
    Ok(())
}

/// Mark a draft PR ready for review. GitHub exposes this write only through
/// GraphQL, so use the REST response's node ID with the native GraphQL API.
/// Returns true when a draft was changed, false when it was already ready.
pub async fn mark_ready(client: &Octocrab, repo: &GitHubRepo, number: u64) -> Result<bool> {
    let pr = client
        .pulls(&repo.owner, &repo.repo)
        .get(number)
        .await
        .with_context(|| format!("failed to fetch PR #{number}"))?;
    if !pr.draft.unwrap_or(false) {
        return Ok(false);
    }
    let node_id = pr
        .node_id
        .ok_or_else(|| anyhow::anyhow!("PR #{number} has no GitHub node ID"))?;
    let payload = ready_payload(&node_id);
    let response: serde_json::Value = client
        .graphql(&payload)
        .await
        .with_context(|| format!("failed to mark PR #{number} ready for review"))?;
    validate_ready_response(&response, number)?;
    Ok(true)
}

fn ready_payload(node_id: &str) -> serde_json::Value {
    json!({
        "query": "mutation($pullRequestId: ID!) { markPullRequestReadyForReview(input: { pullRequestId: $pullRequestId }) { pullRequest { number isDraft } } }",
        "variables": { "pullRequestId": node_id }
    })
}

fn validate_ready_response(response: &serde_json::Value, number: u64) -> Result<()> {
    if let Some(errors) = response.get("errors").and_then(|v| v.as_array())
        && !errors.is_empty()
    {
        let messages = errors
            .iter()
            .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(anyhow::anyhow!(
            "GitHub rejected ready-for-review for PR #{number}: {}",
            if messages.is_empty() {
                "unknown GraphQL error"
            } else {
                &messages
            }
        ));
    }
    let pull = response
        .pointer("/data/markPullRequestReadyForReview/pullRequest")
        .ok_or_else(|| {
            anyhow::anyhow!("GitHub returned no pull request after marking PR #{number} ready")
        })?;
    if pull.get("isDraft").and_then(|v| v.as_bool()) != Some(false) {
        return Err(anyhow::anyhow!(
            "PR #{number} is still a draft after ready-for-review mutation"
        ));
    }
    Ok(())
}

/// Fetch the coarse state of a PR by number.
pub async fn fetch_state(client: &Octocrab, repo: &GitHubRepo, number: u64) -> Result<PrState> {
    let pr = client
        .pulls(&repo.owner, &repo.repo)
        .get(number)
        .await
        .with_context(|| format!("failed to fetch PR #{number}"))?;
    Ok(classify(&pr))
}

/// Find an existing PR whose head branch is `head` (any state). Returns its
/// number if present.
pub async fn find_by_head(client: &Octocrab, repo: &GitHubRepo, head: &str) -> Result<Option<u64>> {
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

// ---------------------------------------------------------------------------
// PR detail for the TUI: merge readiness + CI checks.
// ---------------------------------------------------------------------------

/// Outcome of a single CI check run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckOutcome {
    Passed,
    Failed,
    Running,
    Skipped,
    Neutral,
}

/// One CI check on the PR's head commit.
#[derive(Debug, Clone)]
pub struct CheckLine {
    pub name: String,
    pub outcome: CheckOutcome,
}

/// Whether the PR can be merged, with a human-readable reason.
#[derive(Debug, Clone)]
pub struct Verdict {
    pub ready: bool,
    pub summary: String,
}

/// Everything the TUI's right pane shows about a PR.
#[derive(Debug, Clone)]
pub struct PrDetail {
    pub number: u64,
    pub title: String,
    pub state: PrState,
    pub mergeable_state: &'static str,
    pub verdict: Verdict,
    pub checks: Vec<CheckLine>,
    pub url: String,
}

fn classify_check(conclusion: Option<&str>) -> CheckOutcome {
    match conclusion {
        None => CheckOutcome::Running, // not completed yet
        Some("success") => CheckOutcome::Passed,
        Some("skipped") => CheckOutcome::Skipped,
        Some("neutral") => CheckOutcome::Neutral,
        // failure, timed_out, cancelled, action_required, stale, startup_failure, ...
        Some(_) => CheckOutcome::Failed,
    }
}

fn mergeable_state_label(s: Option<&MergeableState>) -> &'static str {
    match s {
        Some(MergeableState::Behind) => "behind",
        Some(MergeableState::Blocked) => "blocked",
        Some(MergeableState::Clean) => "clean",
        Some(MergeableState::Dirty) => "dirty",
        Some(MergeableState::Draft) => "draft",
        Some(MergeableState::HasHooks) => "has_hooks",
        Some(MergeableState::Unstable) => "unstable",
        _ => "unknown",
    }
}

fn derive_verdict(pr: &PullRequest, state: PrState, checks: &[CheckLine]) -> Verdict {
    let failing = checks
        .iter()
        .filter(|c| c.outcome == CheckOutcome::Failed)
        .count();
    let running = checks
        .iter()
        .filter(|c| c.outcome == CheckOutcome::Running)
        .count();
    let suffix = if checks.is_empty() {
        String::new()
    } else {
        let passed = checks
            .iter()
            .filter(|c| c.outcome == CheckOutcome::Passed)
            .count();
        format!("  ({passed} passed, {failing} failing, {running} running)")
    };
    let ready = |s: &str| Verdict {
        ready: true,
        summary: format!("{s}{suffix}"),
    };
    let no = |s: &str| Verdict {
        ready: false,
        summary: format!("{s}{suffix}"),
    };

    match state {
        PrState::Merged => {
            return Verdict {
                ready: false,
                summary: "Merged".into(),
            };
        }
        PrState::Closed => {
            return Verdict {
                ready: false,
                summary: "Closed".into(),
            };
        }
        _ => {}
    }
    if pr.draft.unwrap_or(false) {
        return no("Draft — mark ready to merge");
    }
    match pr.mergeable_state {
        Some(MergeableState::Clean) | Some(MergeableState::HasHooks) => ready("Ready to merge"),
        Some(MergeableState::Blocked) => no("Blocked — required checks or reviews pending"),
        Some(MergeableState::Unstable) => no("Mergeable, but checks are failing/pending"),
        Some(MergeableState::Dirty) => no("Conflicts with the base branch"),
        Some(MergeableState::Behind) => no("Behind base — run `stacker sync`"),
        Some(MergeableState::Draft) => no("Draft — mark ready to merge"),
        _ => no("Mergeability unknown — GitHub is still computing; refresh"),
    }
}

/// Fetch full PR detail (merge readiness + check runs on the head commit).
pub async fn fetch_detail(client: &Octocrab, repo: &GitHubRepo, number: u64) -> Result<PrDetail> {
    let pr = client
        .pulls(&repo.owner, &repo.repo)
        .get(number)
        .await
        .with_context(|| format!("failed to fetch PR #{number}"))?;

    let head_sha = pr.head.sha.clone();
    // Checks may be unavailable (no CI, or token lacks scope); treat as empty.
    let checks: Vec<CheckLine> = match client
        .checks(&repo.owner, &repo.repo)
        .list_check_runs_for_git_ref(Commitish(head_sha))
        .send()
        .await
    {
        Ok(list) => list
            .check_runs
            .into_iter()
            .map(|r| CheckLine {
                name: r.name,
                outcome: classify_check(r.conclusion.as_deref()),
            })
            .collect(),
        Err(_) => Vec::new(),
    };

    let state = classify(&pr);
    let verdict = derive_verdict(&pr, state, &checks);

    Ok(PrDetail {
        number,
        title: pr.title.clone().unwrap_or_default(),
        state,
        mergeable_state: mergeable_state_label(pr.mergeable_state.as_ref()),
        verdict,
        checks,
        url: pr
            .html_url
            .as_ref()
            .map(|u| u.to_string())
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_payload_uses_node_id_variable() {
        let payload = ready_payload("PR_node_123");
        assert_eq!(payload["variables"]["pullRequestId"], "PR_node_123");
        assert!(
            payload["query"]
                .as_str()
                .unwrap()
                .contains("markPullRequestReadyForReview")
        );
    }

    #[test]
    fn ready_response_requires_non_draft_pr() {
        let ok = json!({
            "data": { "markPullRequestReadyForReview": { "pullRequest": {
                "number": 7, "isDraft": false
            } } }
        });
        assert!(validate_ready_response(&ok, 7).is_ok());

        let still_draft = json!({
            "data": { "markPullRequestReadyForReview": { "pullRequest": {
                "number": 7, "isDraft": true
            } } }
        });
        assert!(validate_ready_response(&still_draft, 7).is_err());
    }

    #[test]
    fn ready_response_reports_graphql_errors() {
        let response = json!({ "errors": [{ "message": "not authorized" }] });
        let error = validate_ready_response(&response, 7)
            .unwrap_err()
            .to_string();
        assert!(error.contains("not authorized"));
    }
}
