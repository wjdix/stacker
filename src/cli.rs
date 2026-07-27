//! Command-line interface definition.

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum MergeStrategy {
    Merge,
    Squash,
    Rebase,
}

#[derive(Parser, Debug)]
#[command(
    name = "stacker",
    version,
    about = "Stacked GitHub pull requests for Jujutsu repositories"
)]
pub struct Cli {
    /// Subcommand to run. With none, opens the interactive TUI.
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Print foundational info: resolved repo key, trunk commit, and config.
    Info,
    /// Show each tracked stack, ordered, with live PR state.
    Status,
    /// Create/update PRs for a stack with chained base branches.
    Submit {
        /// Stack name; if omitted, inferred from the current change (@).
        stack: Option<String>,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Start a NEW stack: a fresh change on trunk (optionally in a new workspace).
    Create {
        /// Name for the new stack.
        name: String,
        /// Create the change in a new jj workspace at this path (name derived from it).
        #[arg(long, value_name = "PATH")]
        workspace: Option<std::path::PathBuf>,
        /// Description for the new change.
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Adopt the current trunk→@ chain as a named stack (discovers PRs if reachable).
    Track {
        /// Name for the stack.
        name: String,
    },
    /// Stop tracking a stack (forgets metadata; leaves jj and PRs untouched).
    Untrack {
        /// Name of the stack to stop tracking.
        name: String,
    },
    /// Add a new empty change on top of an existing stack and check it out.
    Add {
        /// Stack name; if omitted, inferred from the current change (@).
        stack: Option<String>,
        /// Description for the new change.
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Fetch trunk, rebase a stack onto it, and update its PRs.
    Sync {
        /// Stack name; if omitted, inferred from the current change (@).
        stack: Option<String>,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
        /// Don't fetch trunk first; rebase onto the locally-known trunk.
        #[arg(long)]
        no_fetch: bool,
    },
    /// Merge the ready bottom PR, land it, and optionally continue through the stack.
    Merge {
        /// Stack name; if omitted, inferred from the current change (@).
        stack: Option<String>,
        /// Shell command whose exit status says whether the bottom PR is ready (0 = ready).
        #[arg(long, value_name = "COMMAND")]
        check: String,
        /// Keep merging bottom-to-top until the stack is empty or the check fails.
        #[arg(long)]
        all: bool,
        /// GitHub merge method.
        #[arg(long, value_enum, default_value_t = MergeStrategy::Squash)]
        method: MergeStrategy,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// If the bottom PR is merged, drop it and restack the rest onto trunk.
    Land {
        /// Stack name; if omitted, inferred from the current change (@).
        stack: Option<String>,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// If the bottom PR was closed without merging, abandon it and restack the rest onto trunk.
    Drop {
        /// Stack name; if omitted, inferred from the current change (@).
        stack: Option<String>,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Abandon EVERY change in a stack, delete its local bookmarks, and stop tracking it.
    Abandon {
        /// Stack name; if omitted, inferred from the current change (@).
        stack: Option<String>,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
}
