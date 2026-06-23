//! Command-line interface definition.

use clap::{Parser, Subcommand};

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
}
