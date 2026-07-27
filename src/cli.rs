use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};

use crate::archive;
use crate::config::{self, EncryptionMode};
use crate::crypto;
use crate::doctor;
use crate::vault::Vault;

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

    /// Diagnose provider coverage and vault readiness.
    Doctor(DoctorArgs),

    /// Create an incremental recovery point.
    Backup(OutputArgs),

    /// List completed recovery points.
    Snapshots(OutputArgs),

    /// Verify a recovery point.
    Verify(VerifyArgs),

    /// Recover a recovery point into an empty directory.
    Recover(RecoverArgs),
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Filesystem directory that will hold the vault.
    #[arg(long, value_name = "DIRECTORY")]
    pub target: Option<PathBuf>,

    /// Vault encryption mode.
    #[arg(long, value_enum, default_value_t = EncryptionMode::None)]
    pub encryption: EncryptionMode,

    /// Existing age X25519 identity file; otherwise a new recovery identity is generated.
    #[arg(long, value_name = "FILE", requires = "encryption")]
    pub age_identity_file: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print the resolved configuration path.
    Path,

    /// Print and validate the active configuration.
    Show,
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Emit a stable machine-readable report.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct OutputArgs {
    /// Emit stable machine-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// Snapshot ID or `latest`.
    #[arg(default_value = "latest")]
    pub snapshot: String,

    /// Only check manifest structure and object presence.
    #[arg(long)]
    pub quick: bool,

    /// Emit stable machine-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RecoverArgs {
    /// Snapshot ID or `latest`.
    #[arg(default_value = "latest")]
    pub snapshot: String,

    /// Empty directory into which files will be recovered.
    #[arg(long, required = true, value_name = "DIRECTORY")]
    pub to: PathBuf,

    /// Emit stable machine-readable output.
    #[arg(long)]
    pub json: bool,
}

pub fn run(cli: Cli) -> Result<()> {
    let config_path = cli.config.unwrap_or(config::default_config_path()?);

    match cli.command {
        Command::Init(args) => {
            let target = args.target.unwrap_or(config::default_vault_path()?);
            let prepared = crypto::prepare_encryption(
                args.encryption,
                &config_path,
                args.age_identity_file.as_deref(),
            )?;
            let created = match config::initialize(&config_path, &target, prepared.config) {
                Ok(created) => created,
                Err(error) => {
                    if let Some(path) = prepared.generated_identity_file {
                        let _ = std::fs::remove_file(path);
                    }
                    return Err(error);
                }
            };
            Vault::open(&created)?;
            println!("Initialized Akeep vault {}", created.vault.id);
            println!("Config: {}", config_path.display());
            println!("Target: {}", created.target.path.display());
            println!("Encryption: {}", created.encryption.mode);
            if let Some(path) = created.encryption.identity_file {
                println!("Recovery identity: {}", path.display());
                println!("Back up this identity separately; losing it makes recovery impossible.");
            }
        }
        Command::Config { command } => match command {
            ConfigCommand::Path => println!("{}", config_path.display()),
            ConfigCommand::Show => {
                let active = config::Config::load(&config_path)?;
                print!("{}", toml::to_string_pretty(&active)?);
            }
        },
        Command::Doctor(args) => {
            let active = config::Config::load(&config_path)?;
            let report = doctor::inspect(&config_path, &active);
            if args.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                doctor::print_human(&report);
            }
            if !report.healthy {
                bail!("doctor found one or more blocking problems");
            }
        }
        Command::Backup(args) => {
            let active = config::Config::load(&config_path)?;
            let report = archive::backup(&config_path, &active)?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("Created recovery point {}", report.snapshot_id);
                println!("Files: {}", report.files);
                println!("Logical bytes: {}", report.logical_bytes);
                println!("Unique objects: {}", report.unique_objects);
                println!(
                    "New objects: {} ({} stored bytes)",
                    report.new_objects, report.new_stored_bytes
                );
            }
        }
        Command::Snapshots(args) => {
            let active = config::Config::load(&config_path)?;
            let snapshots = archive::snapshots(&active)?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&snapshots)?);
            } else if snapshots.is_empty() {
                println!("No recovery points.");
            } else {
                for snapshot in snapshots {
                    println!(
                        "{}  {}  {} files  {} logical bytes  {} stored bytes",
                        snapshot.snapshot_id,
                        snapshot.hostname,
                        snapshot.files,
                        snapshot.logical_bytes,
                        snapshot.stored_bytes
                    );
                }
            }
        }
        Command::Verify(args) => {
            let active = config::Config::load(&config_path)?;
            let report = archive::verify(&active, &args.snapshot, !args.quick)?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "Verified recovery point {} ({}, {} files, {} bytes)",
                    report.snapshot_id,
                    if report.full { "full" } else { "quick" },
                    report.files,
                    report.logical_bytes
                );
            }
        }
        Command::Recover(args) => {
            let active = config::Config::load(&config_path)?;
            let report = archive::recover(&active, &args.snapshot, &args.to)?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("Recovered {}", report.snapshot_id);
                println!("Target: {}", report.target.display());
                println!("Files: {}", report.files);
                println!("Logical bytes: {}", report.logical_bytes);
            }
        }
    }

    Ok(())
}
