//! `stacker merge [stack] --check <command> [--all]` — merge a stack bottom-up.
//!
//! The user-provided command decides whether the current bottom PR is ready:
//! exit 0 merges it, while any other exit status stops without error. After a
//! merge, the existing `land` workflow fetches trunk and restacks the remainder;
//! the new bottom is then marked ready for review through GitHub's GraphQL API.

use std::io::Write;
use std::process::Command;

use anyhow::{Context, Result, anyhow};
use octocrab::params::pulls::MergeMethod;

use crate::cli::MergeStrategy;
use crate::commands::land;
use crate::config::Config;
use crate::github::client;
use crate::github::pr::{self, MergeCandidate, PrState};
use crate::github::remote::{self, GitHubRepo};
use crate::jj::{repo, trunk};
use crate::paths;
use crate::stack;
use crate::store::Store;

struct Bottom {
    number: u64,
    change_id: String,
    short_change_id: String,
    summary: String,
    candidate: MergeCandidate,
}

pub async fn run(
    stack_name: Option<String>,
    check: String,
    all: bool,
    method: MergeStrategy,
    yes: bool,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    crate::jj::snapshot::snapshot_working_copy(&cwd);

    let loaded = repo::load(&cwd).await?;
    let state_dir = paths::repo_state_dir(loaded.repo_path())?;
    let config = Config::load(&state_dir)?;
    let store = Store::load(&state_dir)?;
    let name = match stack_name {
        Some(name) => name,
        None => stack::infer_stack_name(&loaded, &store)?,
    };
    let gh_repo = remote::resolve(&loaded.repo, &config.remote)?;
    let github = client::build()?;
    let stack_size = store
        .stacks
        .get(&name)
        .ok_or_else(|| anyhow!("no such stack: {name}"))?
        .members
        .len();
    drop(loaded);

    let mut confirmed = yes;
    let mut merged = Vec::new();

    loop {
        let Some(bottom) = load_bottom(&cwd, &state_dir, &config, &name, &github, &gh_repo).await?
        else {
            println!("Stack '{name}' is fully landed.");
            break;
        };

        match bottom.candidate.state {
            PrState::Merged => {
                println!(
                    "Bottom PR #{} is already merged; landing it.",
                    bottom.number
                );
            }
            PrState::Closed => {
                return Err(anyhow!(
                    "bottom PR #{} is closed without merging; use `stacker drop {name}`",
                    bottom.number
                ));
            }
            PrState::Draft => {
                return Err(anyhow!(
                    "bottom PR #{} is still a draft; mark it ready for review before merging",
                    bottom.number
                ));
            }
            PrState::Open => {
                println!(
                    "Checking whether bottom PR #{} is ready: {check}",
                    bottom.number
                );
                if !run_check(&check, &name, &gh_repo, &bottom)? {
                    println!(
                        "Bottom PR #{} is not ready; readiness command exited non-zero.",
                        bottom.number
                    );
                    break;
                }

                if !confirmed {
                    print_plan(&name, &bottom, all, stack_size, method);
                    if !confirm()? {
                        println!("Aborted.");
                        return Ok(());
                    }
                    confirmed = true;
                }

                println!("Merging bottom PR #{} via GitHub API...", bottom.number);
                pr::merge(
                    &github,
                    &gh_repo,
                    bottom.number,
                    &bottom.candidate.head_sha,
                    merge_method(method),
                )
                .await?;
                println!("Bottom PR #{} merged.", bottom.number);
                merged.push(bottom.number);
            }
        }

        // `land` verifies the remote merged state before changing local metadata,
        // then fetches trunk, removes the bottom, rebases, and updates all PRs.
        if let Err(error) = land::run(Some(name.clone()), true).await {
            return Err(error.context(format!(
                "PR #{} was merged, but landing failed; recover with `stacker land {name}`",
                bottom.number
            )));
        }

        let Some(next) = load_bottom(&cwd, &state_dir, &config, &name, &github, &gh_repo).await?
        else {
            println!("Stack '{name}' is fully landed.");
            break;
        };
        match pr::mark_ready(&github, &gh_repo, next.number).await {
            Ok(true) => println!("New bottom PR #{} is ready for review.", next.number),
            Ok(false) => println!(
                "New bottom PR #{} was already ready for review.",
                next.number
            ),
            Err(error) => {
                return Err(error.context(format!(
                    "landed PR #{}, but failed to mark new bottom PR #{} ready",
                    bottom.number, next.number
                )));
            }
        }

        if !all {
            println!("Stopped after one PR. New bottom is #{}.", next.number);
            break;
        }
    }

    if !merged.is_empty() {
        println!(
            "Merged {} PR(s): {}",
            merged.len(),
            merged
                .iter()
                .map(|number| format!("#{number}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

async fn load_bottom(
    cwd: &std::path::Path,
    state_dir: &std::path::Path,
    config: &Config,
    name: &str,
    github: &octocrab::Octocrab,
    gh_repo: &GitHubRepo,
) -> Result<Option<Bottom>> {
    let loaded = repo::load(cwd).await?;
    let store = Store::load(state_dir)?;
    let Some(stored_stack) = store.stacks.get(name) else {
        return Ok(None);
    };
    if stored_stack.members.is_empty() {
        return Ok(None);
    }
    let trunk_id = trunk::resolve(&loaded.repo, &config.trunk, &config.remote).ok();
    let resolved = stack::resolve(&loaded.repo, &store, name, &config.trunk, trunk_id.as_ref())?;
    let Some(entry) = resolved.entries.first() else {
        return Ok(None);
    };
    let number = entry
        .pr_number
        .ok_or_else(|| anyhow!("bottom change {} has no PR", entry.short_change_id))?;
    let candidate = pr::fetch_merge_candidate(github, gh_repo, number).await?;
    Ok(Some(Bottom {
        number,
        change_id: entry.change_id.clone(),
        short_change_id: entry.short_change_id.clone(),
        summary: entry.summary.clone(),
        candidate,
    }))
}

fn run_check(command: &str, stack_name: &str, repo: &GitHubRepo, bottom: &Bottom) -> Result<bool> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let status = Command::new(&shell)
        .args(["-c", command])
        .env("STACKER_PR_NUMBER", bottom.number.to_string())
        .env("STACKER_PR_URL", &bottom.candidate.url)
        .env("STACKER_REPO", format!("{}/{}", repo.owner, repo.repo))
        .env("STACKER_STACK", stack_name)
        .env("STACKER_CHANGE_ID", &bottom.change_id)
        .env("STACKER_HEAD_SHA", &bottom.candidate.head_sha)
        .status()
        .with_context(|| format!("failed to run readiness command with shell '{shell}'"))?;
    Ok(status.success())
}

fn merge_method(method: MergeStrategy) -> MergeMethod {
    match method {
        MergeStrategy::Merge => MergeMethod::Merge,
        MergeStrategy::Squash => MergeMethod::Squash,
        MergeStrategy::Rebase => MergeMethod::Rebase,
    }
}

fn method_label(method: MergeStrategy) -> &'static str {
    match method {
        MergeStrategy::Merge => "merge",
        MergeStrategy::Squash => "squash",
        MergeStrategy::Rebase => "rebase",
    }
}

fn print_plan(name: &str, bottom: &Bottom, all: bool, stack_size: usize, method: MergeStrategy) {
    println!("Plan for stack '{name}':");
    println!(
        "  • {} merge PR #{} ({})  {}",
        method_label(method),
        bottom.number,
        bottom.short_change_id,
        bottom.summary
    );
    println!("  • land it and mark the new bottom ready for review");
    if all {
        println!(
            "  • continue through up to {} PR(s), stopping when the readiness command fails",
            stack_size
        );
    } else {
        println!("  • stop after this bottom PR");
    }
}

fn confirm() -> Result<bool> {
    print!("Proceed? [y/N] ");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim().to_lowercase().as_str(), "y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_strategy_maps_to_octocrab_method() {
        assert!(matches!(
            merge_method(MergeStrategy::Merge),
            MergeMethod::Merge
        ));
        assert!(matches!(
            merge_method(MergeStrategy::Squash),
            MergeMethod::Squash
        ));
        assert!(matches!(
            merge_method(MergeStrategy::Rebase),
            MergeMethod::Rebase
        ));
    }

    #[test]
    fn readiness_command_receives_environment() {
        let bottom = Bottom {
            number: 42,
            change_id: "change-id".into(),
            short_change_id: "change".into(),
            summary: "summary".into(),
            candidate: MergeCandidate {
                state: PrState::Open,
                head_sha: "abc123".into(),
                url: "https://github.com/o/r/pull/42".into(),
            },
        };
        let repo = GitHubRepo {
            owner: "o".into(),
            repo: "r".into(),
        };
        assert!(run_check(
            "test \"$STACKER_PR_NUMBER\" = 42 && test \"$STACKER_REPO\" = o/r && test \"$STACKER_HEAD_SHA\" = abc123",
            "stack",
            &repo,
            &bottom,
        )
        .unwrap());
        assert!(!run_check("exit 3", "stack", &repo, &bottom).unwrap());
    }
}
