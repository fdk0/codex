use anyhow::Result;
use clap::Parser;
use codex_agent_dashboard::Cli;

#[tokio::main]
async fn main() -> Result<()> {
    codex_agent_dashboard::run(Cli::parse()).await
}
