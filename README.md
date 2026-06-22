# stacker

Fast, compiled CLI for managing **stacked GitHub pull requests** in **Jujutsu (jj)** repositories.

A *stack* is a linear chain of jj changes, each tagged with a bookmark, each backing one GitHub PR, each based on the one below it (the bottom PR targets trunk).

## Design

- **Native jj** via in-process `jj-lib` (no shelling out for repo ops). Pinned to `jj-lib =0.41.0` to match the installed `jj 0.41`; a startup guard refuses to run on a version mismatch to avoid on-disk format corruption.
- **Native GitHub** via the `octocrab` API crate. Token resolved from `GITHUB_TOKEN`, then `GH_TOKEN`, then `gh auth token`. `owner/repo` parsed from the jj git remote URL.
- **Named, multiple stacks** per repo. Membership is stored; **order is derived from the jj commit graph**.
- **Metadata** lives in a global, per-repo state dir (keyed by the canonical `.jj/repo` path), so it works across jj workspaces and is never committed.
- **First-class conflicts**: a rebase never stops; conflicts are recorded inside commits. stacker skips conflicted changes (and everything above them), reports them, and exits non-zero.

## Commands

| Command | Description |
|---|---|
| `stacker` (no args) | Open the interactive **TUI** (see below). |
| `stacker info` | Resolved repo key, trunk commit, config. |
| `stacker status` | Each tracked stack, ordered, with bookmark, PR #, live PR state, conflict markers. |
| `stacker create <name> [--workspace <path>] [-m msg]` | Start a **new** stack: a fresh change on trunk. With `--workspace <path>` the change is created in a new jj workspace at that path (name derived from it), leaving the current workspace untouched. |
| `stacker track <name>` | Adopt **existing** changes as a named stack. Members are the **bookmarked PR heads** between trunk and `@` (one bookmark = one PR); commits between bookmarks are that PR's content, not separate members. Discovers each PR (number + base) when GitHub is reachable. Read-only. |
| `stacker untrack <name>` | Stop tracking a stack: forget stacker's metadata. Leaves jj changes, bookmarks, and PRs untouched. |
| `stacker add [stack] [-m msg]` | Add a new empty change on top of an existing stack and check it out. |
| `stacker submit [stack] [--yes]` | Create/update draft PRs bottom→top with chained bases; force-push (lease-safe); maintain a stack-navigation comment. Shows a plan and confirms first. |
| `stacker sync [stack] [--yes] [--no-fetch]` | Fetch trunk, rebase the whole stack onto it, then update all PRs. `--no-fetch` rebases onto the locally-known trunk. |
| `stacker land [stack] [--yes]` | If the bottom PR is merged, fetch trunk, drop the bottom, and restack the rest. |

Stack selection defaults to the stack containing the current change (`@`); pass a name to override.

## TUI

Running `stacker` with no arguments opens an interactive terminal UI. It opens on the stack containing `@` (or, if `@` isn't in a tracked stack, the first one). With multiple tracked stacks, the header shows `Stack: <name> [i/n]` and **`Tab`/`Shift-Tab`** cycle between them (re-resolving and re-fetching the selected one):

- **Left pane** — the stack, head on top: change-id, summary, bookmark, and `PR #`. The bullet is color-coded: **green** = mergeable now, **blue** = clean but must wait for the PRs below it (or already merged), **yellow** = not ready, **red ⚠** = conflict.
- **Right pane** — a compact PR-status header on top (title/state, ready-to-merge verdict, and a one-line check summary with failing names; press `c` to expand the full check list), and below it the selected change's **diff**, rendered with **difftastic** (syntax-aware, inline) and scrollable with `PgDn`/`PgUp`. PR data loads in the background (`◌ Loading…`) without blocking navigation; the diff loads per selection.
- **Background auto-refresh.** While the TUI is open it re-checks PR status/checks every 30s in the background (no input blocking); a `⟳ checking…` footer indicator shows when a refresh is in flight. `r` forces an immediate refresh.
- **Bell on bottom ready/merged.** When the bottom of the stack (the trunk-nearest PR) becomes *ready to merge* or *merged*, the terminal bell rings once (on the transition) and the footer notes it — so you can leave the TUI open and get pinged when the stack is actionable.
- **Stack-aware merge readiness.** A PR is only shown as *ready to merge now* (green) when every PR **below** it has merged. Higher PRs that GitHub reports as "clean" (they target the PR beneath them, not trunk) are shown as "Ready, but merge the PR(s) below it first" — because stacks must merge from the bottom up.
- **Keys**: `↑/↓` (or `j/k`) move · `J`/`K` scroll diff · `c` expand/collapse checks · `Tab`/`Shift-Tab` switch stack · `o` open PR in browser · `p` submit (open/update PRs) · `s` sync · `l` land · `a` add change · `r` refresh · `q`/`Esc` quit.

The diff uses [difftastic](https://difftastic.wilfred.me.uk/) via `jj diff --tool difft` (inline, color), falling back to a plain unified diff if `difft` isn't installed.

`s`/`l`/`a` run `stacker sync`/`land`/`add` on the current stack: the TUI suspends, the command runs in the normal terminal (showing its plan and confirmation prompt), then the TUI re-opens and reloads the updated stack. Requires a terminal and GitHub auth; for scripting use the subcommands instead.

## Configuration

Per-repo `config.toml` in the state dir (`stacker info` prints the path). All optional:

```toml
trunk = "main"           # rebase target bookmark
remote = "origin"        # git remote
bookmark_prefix = ""     # e.g. "wd/" → bookmarks like wd/add-login-form
```

## Build

```sh
cargo build --release    # no protoc needed; first build pulls jj-lib + gix
cargo test
```

## Install

```sh
cargo install --path .           # builds release and installs to ~/.cargo/bin/stacker
cargo install --path . --force   # reinstall after changes
```

Ensure `~/.cargo/bin` is on your `PATH`. Because stacker links `jj-lib =0.41`, reinstall after a `jj` upgrade so the two stay in lockstep (the startup version guard refuses on a mismatch).

## Notes / limitations

- **Working-copy handling.** Commands that read or rewrite `@`'s content (`submit`, `sync`, `land`, `new`) wrap their work in jj's normal working-copy lifecycle: they **snapshot** first (folding any uncommitted on-disk edits into `@`) and **refresh** the working copy to the head operation afterward (so the `jj` CLI never reports a stale working copy). The snapshot is the one place stacker intentionally shells out to `jj` — once, quietly — because faithful snapshotting (gitignore, auto-track, file-size limits, fsmonitor) lives in the jj CLI, not `jj-lib`; the ~10ms cost is negligible against these commands' network IO. Read-only commands (`status`, `info`) stay pure `jj-lib` and never shell out.
- New commits created by `stacker new` inherit the **stack head's author identity** (jj-lib's library defaults carry no `user.name`/`user.email`; config-path discovery lives in the jj CLI, not the library).
- `jj-lib`'s push/fetch spawn `git` internally, so SSH/HTTPS credentials must already work for plain `git` in the repo.
- Keep `jj` and stacker's `jj-lib` in lockstep; after `brew upgrade jj`, rebuild stacker against the matching `jj-lib`.
