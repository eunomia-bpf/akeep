pub mod archive;
pub mod cli;
pub mod config;
pub mod crypto;
pub mod doctor;
pub mod export;
pub mod handoff;
pub mod manifest;
pub mod providers;
pub mod resources;
pub mod scheduler;
pub mod search;
pub mod source;
pub mod storage;
pub mod vault;

use anyhow::Result;

pub fn run(cli: cli::Cli) -> Result<()> {
    cli::run(cli)
}
