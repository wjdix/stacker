//! Command-line interface definition.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "stacker",
    version,
    about = "Stacked GitHub pull requests for Jujutsu repositories"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
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
    /// Register the current trunk→@ chain as a named stack.
    Create {
        /// Name for the new stack.
        name: String,
    },
    /// Import an existing stack (changes + open PRs) under stacker's control.
    Import {
        /// Name for the imported stack.
        name: String,
    },
    /// Stop tracking a stack (forgets metadata; leaves jj and PRs untouched).
    Untrack {
        /// Name of the stack to stop tracking.
        name: String,
    },
    /// Create a new empty change on top of a stack and check it out.
    New {
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
}
