# CLAUDE.md — stacker

Guidance for working on the **stacker** codebase. (For *using* the installed tool, see `README.md` and the `stacker` skill.)

## What this is

A fast, compiled CLI for managing **stacked GitHub pull requests** in **Jujutsu (jj)** repositories. A *stack* is a linear chain of jj changes, each tagged with a bookmark, each backing one PR based on the one below it (bottom → trunk).

Core design rules:
- **Native jj** via in-process `jj-lib` (no shelling out for repo ops).
- **Native GitHub** via `octocrab` (no `gh` subprocess for API calls).
- Fast: load the repo once per command and reuse it (shelling `jj` per op is ~40× slower — see `examples/loadbench.rs`).

## Commands

```sh
cargo build                      # debug build (no protoc needed; jj-lib ships generated protos)
cargo test                       # unit tests (all offline; no network)
cargo clippy                     # must stay warning-clean
cargo install --path . --force   # build release + install to ~/.cargo/bin
cargo run --release --example loadbench -- 100   # jj-lib vs jj-CLI perf bench (run inside a jj repo)
```

## Architecture

```
src/
  main.rs            # tokio entry, clap dispatch
  cli.rs             # clap command/flag definitions
  config.rs          # per-repo config.toml (trunk/remote/bookmark_prefix)
  version_guard.rs   # refuse on installed-jj vs jj-lib minor mismatch
  paths.rs           # global per-repo state dir, keyed by canonical .jj/repo path
  store.rs           # metadata store (stacks + per-change meta), JSON, atomic save
  stack.rs           # the stack model: join stored membership with the live jj graph
  jj/                # in-process jj-lib integration
    repo.rs          #   load workspace+repo at head; find workspace root
    trunk.rs         #   resolve trunk commit (remote bookmark, then local)
    graph.rs         #   change-id→commit resolution, ancestry ordering, summaries
    bookmarks.rs     #   slug/name generation, adopt bookmark-for-commit
    rewrite.rs       #   rebase a stack onto trunk (CommitRewriter + rebase_descendants)
    workspace_ops.rs #   new-change-on-top, working-copy recover-to-head
    snapshot.rs      #   one-shot `jj` snapshot before content-affecting commands
    fetch.rs / push.rs  # native fetch / lease-safe push via jj_lib::git
  github/            # octocrab integration
    client.rs        #   token resolution (env → gh auth token) + client build
    remote.rs        #   parse owner/repo from the git remote URL
    pr.rs            #   create/update/find PRs, fetch state
    nav_comment.rs   #   render + upsert the stack-navigation comment
  commands/          # one module per subcommand (info/status/create/track/untrack/add/submit/sync/land)
                     #   create = new stack · track = adopt existing · add = +change on a stack
  tui.rs             # interactive TUI (no-args): stack + per-PR merge-readiness/checks
```

Command flow: `commands::*::run` → load repo (`jj::repo::load`) → resolve config/store/stack → act. Mutating commands (`submit`/`sync`/`land`/`new`) snapshot first and recover the working copy after.

## Invariants & gotchas (read before changing things)

- **jj-lib is pinned and unstable.** `Cargo.toml` has `jj-lib = "=0.41.0"` to match the installed `jj 0.41`. The API breaks between minor versions; when bumping, expect signature churn and re-verify against `~/.cargo/registry/src/.../jj-lib-<ver>/src`. Keep `version_guard::EXPECTED_JJ_MINOR` in sync with the pin.
- **Change-id encoding.** The store keys changes by `ChangeId::hex()` (standard hex). jj's `change_id` template prints *reverse-hex* (k–z alphabet); use `change_id.normal_hex()` in templates to match. Don't mix them.
- **Stack order is derived, never stored.** `store.stacks[name].members` is an unordered set of change-ids; ordering comes from the jj graph (`graph::order_by_ancestry`). Per-change metadata (bookmark/pr_number/base_branch/nav_comment_id) is keyed by change-id so it survives rebases.
- **Repo key** is the canonical shared `.jj/repo` path (`paths::repo_key`), so metadata is shared across jj workspaces and never committed.
- **Working-copy lifecycle.** stacker is not the jj CLI, so it must snapshot before and recover after itself. `snapshot.rs` shells out to `jj` once (the *only* intentional shell-out for a repo op) because faithful snapshotting lives in the jj CLI, not jj-lib. `workspace_ops::recover` advances the working-copy operation pointer to head to avoid "stale working copy".
- **Conflicts are first-class.** Rebase never stops; `commands::submit` skips conflicted changes and everything above them, reports, and exits non-zero. Preserve this contract.
- **Push is lease-safe.** `jj_lib::git::push_refs` checks the remote-tracking position (`before`) during negotiation. jj-lib's push/fetch spawn `git` internally — that's jj's design, not our shelling.
- **octocrab 0.47.** PR base retarget = `pulls().update(n).base(..)`. `find_by_head` lists `state(All).head("owner:branch")`.

## Conventions

- Errors: `anyhow` with `.context(...)`; jj-lib errors wrapped via `.map_err(|e| anyhow!(e))`.
- Keep `cargo clippy` warning-clean and `cargo test` green before considering work done.
- Pure logic (slug, nav-comment render, base computation, store ops) is unit-tested offline. GitHub write paths are verified manually against a throwaway repo (no live-network tests in the suite).
- Don't reference Claude/AI in jj/git commit messages.

## jj workflow note

This repo uses jj (colocated git). Create a new change before editing (`jj new`), describe after (`jj describe -m ...`). Use bookmarks for PR branches. (Dogfooding: you can manage this repo's own PRs with `stacker`.)
