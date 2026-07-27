use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::config::{self, EncryptionMode};

#[derive(Debug, Parser)]
#[command(name = "akeep", version, about)]
pub struct Cli {
    /// Path to the Akeep configuration file.
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialize a new Akeep vault.
    Init(InitArgs),

    /// Inspect the active configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Filesystem directory that will hold the vault.
    #[arg(long, value_name = "DIRECTORY")]
    pub target: Option<PathBuf>,

    /// Vault encryption mode.
    #[arg(long, value_enum, default_value_t = EncryptionMode::None)]
    pub encryption: EncryptionMode,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print the resolved configuration path.
    Path,

    /// Print and validate the active configuration.
    Show,
}

pub fn run(cli: Cli) -> Result<()> {
    let config_path = cli.config.unwrap_or(config::default_config_path()?);

    match cli.command {
        Command::Init(args) => {
            let target = args.target.unwrap_or(config::default_vault_path()?);
            let created = config::initialize(&config_path, &target, args.encryption)?;
            println!("Initialized Akeep vault {}", created.vault.id);
            println!("Config: {}", config_path.display());
            println!("Target: {}", created.target.path.display());
            println!("Encryption: {}", created.encryption.mode);
        }
        Command::Config { command } => match command {
            ConfigCommand::Path => println!("{}", config_path.display()),
            ConfigCommand::Show => {
                let active = config::Config::load(&config_path)?;
                print!("{}", toml::to_string_pretty(&active)?);
            }
        },
    }

    Ok(())
}
