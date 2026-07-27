mod cli;
mod commands;
mod config;
mod github;
mod jj;
mod paths;
mod stack;
mod store;
mod tui;
mod version_guard;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    version_guard::check()?;

    let Some(command) = cli.command else {
        return tui::run().await;
    };

    match command {
        Command::Info => commands::info::run().await,
        Command::Status => commands::status::run().await,
        Command::Submit { stack, yes } => commands::submit::run(stack, yes).await,
        Command::Create {
            name,
            workspace,
            message,
        } => commands::create::run(name, workspace, message).await,
        Command::Track { name } => commands::track::run(name).await,
        Command::Untrack { name } => commands::untrack::run(name).await,
        Command::Add { stack, message } => commands::add::run(stack, message).await,
        Command::Sync {
            stack,
            yes,
            no_fetch,
        } => commands::sync::run(stack, yes, no_fetch).await,
        Command::Merge {
            stack,
            check,
            all,
            method,
            yes,
        } => commands::merge::run(stack, check, all, method, yes).await,
        Command::Land { stack, yes } => commands::land::run(stack, yes).await,
        Command::Drop { stack, yes } => commands::drop::run(stack, yes).await,
        Command::Abandon { stack, yes } => commands::abandon::run(stack, yes).await,
    }
}
