mod cli;
mod commands;
mod config;
mod github;
mod jj;
mod paths;
mod stack;
mod store;
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

    match cli.command {
        Command::Info => commands::info::run().await,
        Command::Status => commands::status::run().await,
        Command::Submit { stack, yes } => commands::submit::run(stack, yes).await,
        Command::Create { name } => commands::create::run(name).await,
        Command::Import { name } => commands::import::run(name).await,
        Command::Untrack { name } => commands::untrack::run(name).await,
        Command::New { stack, message } => commands::new::run(stack, message).await,
        Command::Sync { stack, yes, no_fetch } => commands::sync::run(stack, yes, no_fetch).await,
        Command::Land { stack, yes } => commands::land::run(stack, yes).await,
    }
}
