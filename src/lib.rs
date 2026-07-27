pub mod cli;
pub mod config;

use anyhow::Result;

pub fn run(cli: cli::Cli) -> Result<()> {
    cli::run(cli)
}
