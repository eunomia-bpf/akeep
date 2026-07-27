pub mod cli;
pub mod config;
pub mod doctor;
pub mod providers;

use anyhow::Result;

pub fn run(cli: cli::Cli) -> Result<()> {
    cli::run(cli)
}
