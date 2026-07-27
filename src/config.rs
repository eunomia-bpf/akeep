use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use clap::ValueEnum;
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const CONFIG_FORMAT_VERSION: u32 = 1;
pub const DEFAULT_CHUNK_SIZE: u64 = 4 * 1024 * 1024;
pub const DEFAULT_COMPRESSION_LEVEL: i32 = 3;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum EncryptionMode {
    #[default]
    None,
}

impl fmt::Display for EncryptionMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("none"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub format_version: u32,
    pub vault: VaultConfig,
    pub target: FilesystemTarget,
    pub archive: ArchiveConfig,
    pub encryption: EncryptionConfig,
    #[serde(default)]
    pub sources: SourceOverrides,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VaultConfig {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemTarget {
    #[serde(rename = "type")]
    pub kind: FilesystemTargetKind,
    pub path: PathBuf,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FilesystemTargetKind {
    Filesystem,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveConfig {
    pub chunk_size: u64,
    pub compression_level: i32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EncryptionConfig {
    pub mode: EncryptionMode,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_home: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_home: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grok_home: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kimi_home: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opencode_share: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opencode_state: Option<PathBuf>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read configuration {}", path.display()))?;
        let config: Self = toml::from_str(&contents)
            .with_context(|| format!("failed to parse configuration {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.format_version != CONFIG_FORMAT_VERSION {
            bail!(
                "unsupported configuration format version {} (expected {})",
                self.format_version,
                CONFIG_FORMAT_VERSION
            );
        }
        if self.archive.chunk_size == 0 {
            bail!("archive.chunk_size must be greater than zero");
        }
        if !(-7..=22).contains(&self.archive.compression_level) {
            bail!("archive.compression_level must be between -7 and 22");
        }
        if self.target.path.as_os_str().is_empty() {
            bail!("target.path must not be empty");
        }
        Ok(())
    }
}

pub fn default_config_path() -> Result<PathBuf> {
    let base = BaseDirs::new().context("could not determine the user configuration directory")?;
    Ok(base.config_dir().join("akeep").join("config.toml"))
}

pub fn default_vault_path() -> Result<PathBuf> {
    let base = BaseDirs::new().context("could not determine the user data directory")?;
    Ok(base
        .data_local_dir()
        .join("akeep")
        .join("vaults")
        .join("default"))
}

pub fn initialize(
    config_path: &Path,
    target_path: &Path,
    encryption: EncryptionMode,
) -> Result<Config> {
    if config_path.exists() {
        bail!(
            "configuration already exists at {}; refusing to overwrite it",
            config_path.display()
        );
    }

    let target = absolute_directory(target_path)?;
    let config = Config {
        format_version: CONFIG_FORMAT_VERSION,
        vault: VaultConfig {
            id: Uuid::new_v4(),
            created_at: Utc::now(),
        },
        target: FilesystemTarget {
            kind: FilesystemTargetKind::Filesystem,
            path: target,
        },
        archive: ArchiveConfig {
            chunk_size: DEFAULT_CHUNK_SIZE,
            compression_level: DEFAULT_COMPRESSION_LEVEL,
        },
        encryption: EncryptionConfig { mode: encryption },
        sources: SourceOverrides::default(),
    };
    config.validate()?;

    let parent = config_path
        .parent()
        .with_context(|| format!("configuration path {} has no parent", config_path.display()))?;
    create_private_directory(parent)?;
    write_private_new_file(config_path, toml::to_string_pretty(&config)?.as_bytes())?;

    Ok(config)
}

fn absolute_directory(path: &Path) -> Result<PathBuf> {
    create_private_directory(path)?;
    fs::canonicalize(path)
        .with_context(|| format!("failed to resolve directory {}", path.display()))
}

fn create_private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("failed to create directory {}", path.display()))?;
    set_private_directory_permissions(path)
}

fn write_private_new_file(path: &Path, contents: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    set_private_file_creation_permissions(&mut options);
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to create file {}", path.display()))?;
    file.write_all(contents)
        .with_context(|| format!("failed to write file {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync file {}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn set_private_file_creation_permissions(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn set_private_file_creation_permissions(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to set permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_format_version() {
        let mut config = sample_config();
        config.format_version = 99;
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_zero_chunk_size() {
        let mut config = sample_config();
        config.archive.chunk_size = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn round_trips_toml() {
        let config = sample_config();
        let encoded = toml::to_string_pretty(&config).unwrap();
        let decoded: Config = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded, config);
    }

    fn sample_config() -> Config {
        Config {
            format_version: CONFIG_FORMAT_VERSION,
            vault: VaultConfig {
                id: Uuid::nil(),
                created_at: DateTime::from_timestamp(0, 0).unwrap(),
            },
            target: FilesystemTarget {
                kind: FilesystemTargetKind::Filesystem,
                path: PathBuf::from("/tmp/akeep-test"),
            },
            archive: ArchiveConfig {
                chunk_size: DEFAULT_CHUNK_SIZE,
                compression_level: DEFAULT_COMPRESSION_LEVEL,
            },
            encryption: EncryptionConfig {
                mode: EncryptionMode::None,
            },
            sources: SourceOverrides::default(),
        }
    }
}
