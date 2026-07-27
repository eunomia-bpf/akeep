pub mod archive;
pub mod cli;
pub mod config;
pub mod crypto;
pub mod doctor;
pub mod manifest;
pub mod providers;
pub mod source;
pub mod vault;

use anyhow::Result;

pub fn run(cli: cli::Cli) -> Result<()> {
    cli::run(cli)
}
