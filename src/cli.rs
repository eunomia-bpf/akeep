use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};

use crate::archive;
use crate::config::{self, EncryptionMode, TargetConfig};
use crate::crypto;
use crate::doctor;
use crate::handoff::{self, HandoffRequest};
use crate::providers::Provider;
use crate::scheduler;
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
    /// Initialize a new Akeep repository.
    Init(InitArgs),

    /// Inspect the active configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },

    /// Show provider coverage and repository readiness.
    Status(StatusArgs),

    /// Save a new version of discovered agent history.
    Commit(CommitArgs),

    /// List committed versions.
    Log(OutputArgs),

    /// Check repository structure and archived content.
    Fsck(FsckArgs),

    /// Restore a version into an empty directory.
    Checkout(CheckoutArgs),

    /// Compare two committed versions.
    Diff(DiffArgs),

    /// Copy the active repository into a self-contained local bundle.
    Clone(CloneArgs),

    /// Manage an optional automatic commit schedule.
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },

    /// Create a reviewed Claude Code ↔ Codex handoff bundle.
    Handoff(HandoffArgs),
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Filesystem directory that will hold the repository.
    #[arg(long, value_name = "DIRECTORY", conflicts_with = "s3_bucket")]
    pub target: Option<PathBuf>,

    /// S3 bucket that will hold the repository.
    #[arg(long, value_name = "BUCKET", conflicts_with = "target")]
    pub s3_bucket: Option<String>,

    /// Relative prefix within the S3 bucket.
    #[arg(long, value_name = "PREFIX", default_value = "akeep")]
    pub s3_prefix: String,

    /// AWS region override.
    #[arg(long, value_name = "REGION", requires = "s3_bucket")]
    pub aws_region: Option<String>,

    /// AWS CLI profile.
    #[arg(long, value_name = "PROFILE", requires = "s3_bucket")]
    pub aws_profile: Option<String>,

    /// S3-compatible endpoint URL.
    #[arg(long, value_name = "URL", requires = "s3_bucket")]
    pub s3_endpoint_url: Option<String>,

    /// AWS CLI executable override.
    #[arg(long, value_name = "FILE", requires = "s3_bucket")]
    pub aws_cli: Option<PathBuf>,

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
pub struct StatusArgs {
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
pub struct CommitArgs {
    /// Short description of this version.
    #[arg(short, long, value_name = "MESSAGE")]
    pub message: Option<String>,

    /// Emit stable machine-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct FsckArgs {
    /// Commit ID, `HEAD`, or an ancestor such as `HEAD~1`.
    #[arg(default_value = "HEAD")]
    pub commit: String,

    /// Only check manifest structure and object presence.
    #[arg(long)]
    pub quick: bool,

    /// Emit stable machine-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CheckoutArgs {
    /// Commit ID, `HEAD`, or an ancestor such as `HEAD~1`.
    #[arg(default_value = "HEAD")]
    pub commit: String,

    /// Recover only one provider (for a smaller, provider-native drill).
    #[arg(long, value_enum)]
    pub provider: Option<Provider>,

    /// Empty directory into which files will be recovered.
    #[arg(long, required = true, value_name = "DIRECTORY")]
    pub to: PathBuf,

    /// Emit stable machine-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct DiffArgs {
    /// Older commit; defaults to `HEAD~1`.
    pub from: Option<String>,

    /// Newer commit; defaults to `HEAD`.
    #[arg(requires = "from")]
    pub to: Option<String>,

    /// Print only change kind and logical path.
    #[arg(long, conflicts_with = "json")]
    pub name_only: bool,

    /// Emit stable machine-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CloneArgs {
    /// New directory for config.toml, repository/, and local state/.
    #[arg(value_name = "DIRECTORY")]
    pub directory: PathBuf,

    /// Emit stable machine-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub enum ScheduleCommand {
    /// Install and start a persistent systemd user timer.
    Install {
        /// Schedule one commit each week.
        #[arg(long, required = true)]
        weekly: bool,

        /// Emit stable machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Report the timer's installation and runtime state.
    Status {
        /// Emit stable machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Disable and remove the timer, preserving configuration and archives.
    Uninstall {
        /// Emit stable machine-readable output.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
pub struct HandoffArgs {
    /// Commit ID, `HEAD`, or an ancestor such as `HEAD~1`.
    #[arg(default_value = "HEAD")]
    pub snapshot: String,

    /// Agent whose archived context is being handed off.
    #[arg(long, value_enum)]
    pub from: Provider,

    /// Agent that will receive the handoff.
    #[arg(long = "for", value_enum)]
    pub for_agent: Provider,

    /// Concrete objective for the receiving agent.
    #[arg(long)]
    pub goal: String,

    /// Established decision; repeat for multiple decisions.
    #[arg(long = "decision")]
    pub decisions: Vec<String>,

    /// Remaining task; repeat for multiple tasks.
    #[arg(long = "open-task")]
    pub open_tasks: Vec<String>,

    /// Known test result or status; repeat for multiple entries.
    #[arg(long = "test-status")]
    pub test_status: Vec<String>,

    /// Repository artifact to hash and list; paths must stay inside the repository.
    #[arg(long = "artifact", value_name = "FILE")]
    pub artifacts: Vec<PathBuf>,

    /// Git repository whose current state should be captured.
    #[arg(long, default_value = ".", value_name = "DIRECTORY")]
    pub repo: PathBuf,

    /// New Markdown bundle to create.
    #[arg(long, required = true, value_name = "FILE")]
    pub to: PathBuf,

    /// Emit a stable machine-readable report.
    #[arg(long)]
    pub json: bool,
}

pub fn run(cli: Cli) -> Result<()> {
    let config_path = cli.config.unwrap_or(config::default_config_path()?);

    match cli.command {
        Command::Init(args) => {
            let target = if let Some(bucket) = args.s3_bucket {
                TargetConfig::S3 {
                    bucket,
                    prefix: args.s3_prefix,
                    region: args.aws_region,
                    profile: args.aws_profile,
                    endpoint_url: args.s3_endpoint_url,
                    aws_cli: args.aws_cli,
                }
            } else {
                TargetConfig::Filesystem {
                    path: args.target.unwrap_or(config::default_vault_path()?),
                }
            };
            let prepared = crypto::prepare_encryption(
                args.encryption,
                &config_path,
                args.age_identity_file.as_deref(),
            )?;
            let generated_identity = prepared.generated_identity_file.clone();
            let created = match config::initialize(&config_path, target, prepared.config) {
                Ok(created) => created,
                Err(error) => {
                    if let Some(path) = generated_identity {
                        let _ = std::fs::remove_file(path);
                    }
                    return Err(error);
                }
            };
            if let Err(error) = Vault::open(&created) {
                let _ = std::fs::remove_file(&config_path);
                let _ = std::fs::remove_dir_all(&created.vault.state_path);
                if let Some(path) = generated_identity {
                    let _ = std::fs::remove_file(path);
                }
                return Err(error).context("repository initialization failed and was rolled back");
            }
            println!("Initialized Akeep repository {}", created.vault.id);
            println!("Config: {}", config_path.display());
            match &created.target {
                TargetConfig::Filesystem { path } => {
                    println!("Target: {}", path.display());
                }
                TargetConfig::S3 { bucket, prefix, .. } => {
                    println!("Target: s3://{bucket}/{prefix}/");
                    if created.encryption.mode == EncryptionMode::None {
                        println!(
                            "Warning: this remote repository is not client-side encrypted; the storage operator can read it."
                        );
                    }
                }
            }
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
        Command::Status(args) => {
            let active = config::Config::load(&config_path)?;
            let report = doctor::inspect(&config_path, &active);
            if args.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                doctor::print_human(&report);
            }
            if !report.healthy {
                bail!("status found one or more blocking problems");
            }
        }
        Command::Commit(args) => {
            let active = config::Config::load(&config_path)?;
            let report = archive::backup(&config_path, &active, args.message.as_deref())?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("Committed agent history {}", report.snapshot_id);
                println!("Files: {}", report.files);
                println!("Logical bytes: {}", report.logical_bytes);
                println!("Unique objects: {}", report.unique_objects);
                println!(
                    "New objects: {} ({} stored bytes)",
                    report.new_objects, report.new_stored_bytes
                );
                println!("Duration: {} ms", report.duration_ms);
            }
        }
        Command::Log(args) => {
            let active = config::Config::load(&config_path)?;
            let snapshots = archive::snapshots(&active)?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&snapshots)?);
            } else if snapshots.is_empty() {
                println!("No commits.");
            } else {
                for snapshot in snapshots {
                    let providers = snapshot
                        .providers
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(",");
                    let message = snapshot.message.as_deref().unwrap_or("-");
                    println!(
                        "{}  {}  [{}]  {} files  {} logical bytes  {} stored bytes  {}  {}",
                        snapshot.snapshot_id,
                        snapshot.hostname,
                        providers,
                        snapshot.files,
                        snapshot.logical_bytes,
                        snapshot.stored_bytes,
                        snapshot.verification,
                        message
                    );
                }
            }
        }
        Command::Fsck(args) => {
            let active = config::Config::load(&config_path)?;
            let report = archive::verify(&active, &args.commit, !args.quick)?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "Checked commit {} ({}, {} files, {} bytes)",
                    report.snapshot_id,
                    if report.full { "full" } else { "quick" },
                    report.files,
                    report.logical_bytes
                );
                println!("Duration: {} ms", report.duration_ms);
            }
        }
        Command::Checkout(args) => {
            let active = config::Config::load(&config_path)?;
            let report = archive::recover(&active, &args.commit, &args.to, args.provider)?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("Checked out {}", report.snapshot_id);
                if let Some(provider) = report.provider {
                    println!("Provider: {provider}");
                }
                println!("Target: {}", report.target.display());
                println!("Files: {}", report.files);
                println!("Logical bytes: {}", report.logical_bytes);
                println!("Duration: {} ms", report.duration_ms);
            }
        }
        Command::Diff(args) => {
            let active = config::Config::load(&config_path)?;
            let (from, to) = match (args.from.as_deref(), args.to.as_deref()) {
                (None, None) => ("HEAD~1", "HEAD"),
                (Some(from), None) => (from, "HEAD"),
                (Some(from), Some(to)) => (from, to),
                (None, Some(_)) => unreachable!("clap requires FROM when TO is present"),
            };
            let report = archive::diff(&active, from, to)?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else if args.name_only {
                for change in report.changes {
                    println!("{}  {}", change.kind, change.logical_path);
                }
            } else {
                println!("Diff {}..{}", report.from, report.to);
                println!(
                    "Files: +{} ~{} -{}",
                    report.files_added, report.files_modified, report.files_removed
                );
                println!(
                    "Logical bytes: {} -> {}",
                    report.bytes_from, report.bytes_to
                );
                if report.changes.is_empty() {
                    println!("No changes.");
                } else {
                    for provider in report.providers {
                        println!(
                            "{}: +{} ~{} -{}, {} -> {} bytes",
                            provider.provider,
                            provider.files_added,
                            provider.files_modified,
                            provider.files_removed,
                            provider.bytes_from,
                            provider.bytes_to
                        );
                    }
                }
            }
        }
        Command::Clone(args) => {
            let active = config::Config::load(&config_path)?;
            let report = archive::clone_repository(&active, &args.directory)?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("Cloned repository to {}", report.destination.display());
                println!("Config: {}", report.config.display());
                println!("Repository objects: {}", report.repository_objects);
                println!("Stored bytes: {}", report.stored_bytes);
                println!("HEAD: {}", report.head);
                println!("Duration: {} ms", report.duration_ms);
            }
        }
        Command::Schedule { command } => {
            let active = config::Config::load(&config_path)?;
            let (action, report, json) = match command {
                ScheduleCommand::Install { weekly, json } => {
                    debug_assert!(weekly);
                    (
                        "Installed",
                        scheduler::install(&config_path, &active)?,
                        json,
                    )
                }
                ScheduleCommand::Status { json } => ("Schedule", scheduler::status(&active)?, json),
                ScheduleCommand::Uninstall { json } => {
                    ("Uninstalled", scheduler::uninstall(&active)?, json)
                }
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("{action}: {}", report.unit_name);
                println!("Installed: {}", report.installed);
                println!("Enabled: {}", report.enabled);
                println!("Active: {}", report.active);
                println!("Service: {}", report.service_path.display());
                println!("Timer: {}", report.timer_path.display());
            }
        }
        Command::Handoff(args) => {
            let active = config::Config::load(&config_path)?;
            let report = handoff::create(
                &active,
                &HandoffRequest {
                    snapshot: args.snapshot,
                    from: args.from,
                    for_agent: args.for_agent,
                    goal: args.goal,
                    decisions: args.decisions,
                    open_tasks: args.open_tasks,
                    test_status: args.test_status,
                    artifacts: args.artifacts,
                    repository: args.repo,
                    output: args.to,
                },
            )?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "Created {} → {} handoff from commit {}",
                    report.from, report.for_agent, report.snapshot_id
                );
                println!("Output: {}", report.output.display());
                println!("Repository: {}", report.repository.display());
                println!("Changed files: {}", report.changed_files);
                println!("Artifacts: {}", report.artifacts);
                println!("Context files: {}", report.context_files);
            }
        }
    }

    Ok(())
}
