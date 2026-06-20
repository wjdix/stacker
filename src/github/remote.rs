//! Deriving the GitHub `owner/repo` from the jj repo's git remote URL.

use anyhow::{Context, Result, anyhow};
use jj_lib::git;
use jj_lib::repo::{ReadonlyRepo, Repo};

/// A GitHub repository coordinate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubRepo {
    pub owner: String,
    pub repo: String,
}

/// Read the fetch URL of `remote` from the repo's git backend and parse it.
pub fn resolve(repo: &ReadonlyRepo, remote: &str) -> Result<GitHubRepo> {
    let git_repo = git::get_git_repo(repo.store())
        .map_err(|e| anyhow!("repo is not backed by git: {e}"))?;
    let gix_remote = git_repo
        .find_remote(remote)
        .with_context(|| format!("git remote '{remote}' not found"))?;
    let url = gix_remote
        .url(gix::remote::Direction::Fetch)
        .or_else(|| gix_remote.url(gix::remote::Direction::Push))
        .ok_or_else(|| anyhow!("git remote '{remote}' has no URL"))?;
    let url_str = url.to_bstring().to_string();
    parse_github_slug(&url_str)
        .ok_or_else(|| anyhow!("could not parse a github owner/repo from remote URL: {url_str}"))
}

/// Parse `owner/repo` from common GitHub remote URL forms:
/// - `git@github.com:owner/repo.git`
/// - `https://github.com/owner/repo.git`
/// - `ssh://git@github.com/owner/repo.git`
pub fn parse_github_slug(url: &str) -> Option<GitHubRepo> {
    let url = url.trim();
    // Strip scheme / scp-like prefix down to the host-relative path.
    let after_host = if let Some(rest) = url.strip_prefix("git@") {
        // git@github.com:owner/repo.git
        rest.split_once(':')?.1
    } else if let Some(rest) = url.split_once("://") {
        // scheme://[user@]host/owner/repo(.git)
        let host_and_path = rest.1;
        host_and_path.split_once('/').map(|(_, p)| p)?
    } else if let Some((_, path)) = url.split_once(':') {
        // host:owner/repo without explicit user
        path
    } else {
        return None;
    };

    let path = after_host.trim_start_matches('/').trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.splitn(2, '/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim();
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some(GitHubRepo {
        owner: owner.to_string(),
        repo: repo.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slug(o: &str, r: &str) -> Option<GitHubRepo> {
        Some(GitHubRepo {
            owner: o.to_string(),
            repo: r.to_string(),
        })
    }

    #[test]
    fn parses_scp_form() {
        assert_eq!(
            parse_github_slug("git@github.com:pagos/stacker.git"),
            slug("pagos", "stacker")
        );
    }

    #[test]
    fn parses_https_form() {
        assert_eq!(
            parse_github_slug("https://github.com/pagos/stacker.git"),
            slug("pagos", "stacker")
        );
        assert_eq!(
            parse_github_slug("https://github.com/pagos/stacker"),
            slug("pagos", "stacker")
        );
    }

    #[test]
    fn parses_ssh_scheme_form() {
        assert_eq!(
            parse_github_slug("ssh://git@github.com/pagos/stacker.git"),
            slug("pagos", "stacker")
        );
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_github_slug("not-a-url"), None);
    }
}
