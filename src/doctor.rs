use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::config::{Config, TargetConfig};
use crate::crypto::CryptoContext;
use crate::providers::{Provider, ProviderSpec, SourceItem, specifications};
use crate::scheduler;
use crate::storage::Storage;

pub const DOCTOR_REPORT_VERSION: u32 = 2;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DoctorReport {
    pub report_version: u32,
    pub healthy: bool,
    pub config_path: PathBuf,
    pub target: String,
    pub storage_versioning: Option<String>,
    pub scheduler_installed: Option<bool>,
    pub encryption_mode: String,
    pub providers: Vec<ProviderInventory>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderInventory {
    pub provider: Provider,
    pub display_name: String,
    pub present: bool,
    pub file_count: u64,
    pub logical_bytes: u64,
    pub skipped_symlinks: u64,
    pub items: Vec<ItemInventory>,
    pub excluded_paths: Vec<PathBuf>,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ItemInventory {
    pub source_path: PathBuf,
    pub logical_path: String,
    pub exists: bool,
    pub file_count: u64,
    pub logical_bytes: u64,
    pub skipped_symlinks: u64,
    pub errors: Vec<String>,
}

pub fn inspect(config_path: &Path, config: &Config) -> DoctorReport {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut storage_versioning = None;
    let scheduler_installed = if cfg!(target_os = "linux") {
        match scheduler::is_installed(config) {
            Ok(installed) => Some(installed),
            Err(error) => {
                warnings.push(format!("could not inspect scheduler state: {error:#}"));
                None
            }
        }
    } else {
        None
    };

    if !config.vault.state_path.is_dir() {
        errors.push(format!(
            "vault state path is not a directory: {}",
            config.vault.state_path.display()
        ));
    }
    let target = match &config.target {
        TargetConfig::Filesystem { path } => {
            if !path.is_dir() {
                errors.push(format!(
                    "vault target is not a directory: {}",
                    path.display()
                ));
            }
            path.display().to_string()
        }
        TargetConfig::S3 { bucket, prefix, .. } => {
            match Storage::from_config(&config.target) {
                Ok(storage) => {
                    if let Err(error) = storage.check_readable() {
                        errors.push(format!("remote vault target is not readable: {error:#}"));
                    } else {
                        match storage.versioning_status() {
                            Ok(status) => {
                                storage_versioning = status;
                                if storage_versioning.as_deref() != Some("Enabled") {
                                    warnings.push(
                                        "S3 bucket versioning is not enabled; remote overwrites have less protection"
                                            .to_string(),
                                    );
                                }
                            }
                            Err(error) => warnings
                                .push(format!("could not check S3 bucket versioning: {error:#}")),
                        }
                    }
                }
                Err(error) => {
                    errors.push(format!("remote vault target is not readable: {error:#}"));
                }
            }
            if config.encryption.mode == crate::config::EncryptionMode::None {
                warnings.push(
                    "remote vault is not client-side encrypted; the storage operator can read archived content"
                        .to_string(),
                );
            }
            format!("s3://{bucket}/{prefix}/")
        }
    };
    if let Err(error) = CryptoContext::from_config(&config.encryption) {
        errors.push(format!("encryption configuration is not usable: {error:#}"));
    }

    let specs = match specifications(config) {
        Ok(specs) => specs,
        Err(error) => {
            errors.push(format!("could not resolve provider paths: {error:#}"));
            Vec::new()
        }
    };

    for spec in &specs {
        for root in &spec.roots {
            if let TargetConfig::Filesystem { path } = &config.target {
                if paths_overlap(path, root) {
                    errors.push(format!(
                        "vault target {} overlaps provider root {}; choose a separate target",
                        path.display(),
                        root.display()
                    ));
                }
            }
            if paths_overlap(&config.vault.state_path, root) {
                errors.push(format!(
                    "vault state path {} overlaps provider root {}; choose a separate state path",
                    config.vault.state_path.display(),
                    root.display()
                ));
            }
        }
    }

    let providers: Vec<_> = specs.iter().map(inventory).collect();
    if providers.iter().all(|provider| !provider.present) {
        warnings.push("no supported provider data was discovered".to_string());
    }
    for provider in &providers {
        if provider.skipped_symlinks > 0 {
            warnings.push(format!(
                "{}: skipped {} symlink(s)",
                provider.provider, provider.skipped_symlinks
            ));
        }
        errors.extend(
            provider
                .errors
                .iter()
                .map(|error| format!("{}: {error}", provider.provider)),
        );
    }

    DoctorReport {
        report_version: DOCTOR_REPORT_VERSION,
        healthy: errors.is_empty(),
        config_path: config_path.to_path_buf(),
        target,
        storage_versioning,
        scheduler_installed,
        encryption_mode: config.encryption.mode.to_string(),
        providers,
        warnings,
        errors,
    }
}

pub fn print_human(report: &DoctorReport) {
    println!("Akeep status");
    println!("Config: {}", report.config_path.display());
    println!("Target: {}", report.target);
    if let Some(status) = &report.storage_versioning {
        println!("Bucket versioning: {status}");
    }
    if let Some(installed) = report.scheduler_installed {
        println!(
            "Automatic schedule: {}",
            if installed {
                "installed"
            } else {
                "not installed"
            }
        );
    }
    println!("Encryption: {}", report.encryption_mode);
    println!("Providers:");
    for provider in &report.providers {
        let status = if provider.present {
            "present"
        } else {
            "not found"
        };
        println!(
            "  {:<12} {:<9} {:>8} files  {:>10}",
            provider.provider,
            status,
            provider.file_count,
            human_bytes(provider.logical_bytes)
        );
        for error in &provider.errors {
            println!("    error: {error}");
        }
    }
    for warning in &report.warnings {
        println!("Warning: {warning}");
    }
    for error in &report.errors {
        println!("Error: {error}");
    }
    println!(
        "Result: {}",
        if report.healthy { "ready" } else { "not ready" }
    );
}

fn inventory(spec: &ProviderSpec) -> ProviderInventory {
    let items: Vec<_> = spec.items.iter().map(inventory_item).collect();
    let errors: Vec<_> = items
        .iter()
        .flat_map(|item| item.errors.iter().cloned())
        .collect();
    ProviderInventory {
        provider: spec.provider,
        display_name: spec.provider.display_name().to_string(),
        present: items.iter().any(|item| item.exists),
        file_count: items.iter().map(|item| item.file_count).sum(),
        logical_bytes: items.iter().map(|item| item.logical_bytes).sum(),
        skipped_symlinks: items.iter().map(|item| item.skipped_symlinks).sum(),
        items,
        excluded_paths: spec.excluded.clone(),
        errors,
    }
}

fn inventory_item(item: &SourceItem) -> ItemInventory {
    if !item.source_path.exists() {
        return ItemInventory {
            source_path: item.source_path.clone(),
            logical_path: item.logical_path.clone(),
            exists: false,
            file_count: 0,
            logical_bytes: 0,
            skipped_symlinks: 0,
            errors: Vec::new(),
        };
    }

    match fs::symlink_metadata(&item.source_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => ItemInventory {
            source_path: item.source_path.clone(),
            logical_path: item.logical_path.clone(),
            exists: true,
            file_count: 0,
            logical_bytes: 0,
            skipped_symlinks: 1,
            errors: vec!["top-level source is a symlink; refusing to follow it".to_string()],
        },
        Ok(metadata) if metadata.is_file() => ItemInventory {
            source_path: item.source_path.clone(),
            logical_path: item.logical_path.clone(),
            exists: true,
            file_count: 1,
            logical_bytes: metadata.len(),
            skipped_symlinks: 0,
            errors: Vec::new(),
        },
        Ok(metadata) if metadata.is_dir() => inventory_directory(item),
        Ok(_) => ItemInventory {
            source_path: item.source_path.clone(),
            logical_path: item.logical_path.clone(),
            exists: true,
            file_count: 0,
            logical_bytes: 0,
            skipped_symlinks: 0,
            errors: vec!["source is neither a regular file nor a directory".to_string()],
        },
        Err(error) => ItemInventory {
            source_path: item.source_path.clone(),
            logical_path: item.logical_path.clone(),
            exists: true,
            file_count: 0,
            logical_bytes: 0,
            skipped_symlinks: 0,
            errors: vec![format!("could not inspect source: {error}")],
        },
    }
}

fn inventory_directory(item: &SourceItem) -> ItemInventory {
    let mut file_count = 0;
    let mut logical_bytes = 0;
    let mut skipped_symlinks = 0;
    let mut errors = Vec::new();

    for entry in WalkDir::new(&item.source_path).follow_links(false) {
        match entry {
            Ok(entry) if entry.file_type().is_file() => match entry.metadata() {
                Ok(metadata) => {
                    file_count += 1;
                    logical_bytes += metadata.len();
                }
                Err(error) => errors.push(format!("{}: {error}", entry.path().display())),
            },
            Ok(entry) if entry.file_type().is_symlink() => {
                skipped_symlinks += 1;
            }
            Ok(_) => {}
            Err(error) => errors.push(error.to_string()),
        }
    }

    ItemInventory {
        source_path: item.source_path.clone(),
        logical_path: item.logical_path.clone(),
        exists: true,
        file_count,
        logical_bytes,
        skipped_symlinks,
        errors,
    }
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    let left = resolve_for_overlap(left);
    let right = resolve_for_overlap(right);
    left.starts_with(&right) || right.starts_with(&left)
}

fn resolve_for_overlap(path: &Path) -> PathBuf {
    if let Ok(resolved) = fs::canonicalize(path) {
        return resolved;
    }

    let mut missing = Vec::new();
    let mut ancestor = path;
    while let (Some(name), Some(parent)) = (ancestor.file_name(), ancestor.parent()) {
        missing.push(name.to_os_string());
        ancestor = parent;
        if let Ok(mut resolved) = fs::canonicalize(ancestor) {
            for component in missing.iter().rev() {
                resolved.push(component);
            }
            return resolved;
        }
    }
    path.to_path_buf()
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use chrono::DateTime;
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;
    use crate::config::{
        ArchiveConfig, EncryptionConfig, EncryptionMode, SourceOverrides, TargetConfig, VaultConfig,
    };

    #[test]
    fn discovers_overridden_provider_content() {
        let temp = TempDir::new().unwrap();
        let claude = temp.path().join("claude");
        let target = temp.path().join("vault");
        fs::create_dir_all(claude.join("projects/demo")).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(claude.join("projects/demo/session.jsonl"), b"hello").unwrap();

        let mut config = sample_config(target);
        config.sources.claude_home = Some(claude);
        config.sources.codex_home = Some(temp.path().join("missing-codex"));
        config.sources.grok_home = Some(temp.path().join("missing-grok"));
        config.sources.kimi_home = Some(temp.path().join("missing-kimi"));
        config.sources.opencode_share = Some(temp.path().join("missing-opencode-share"));
        config.sources.opencode_state = Some(temp.path().join("missing-opencode-state"));

        let report = inspect(Path::new("/config.toml"), &config);
        assert!(report.healthy);
        let claude = &report.providers[0];
        assert!(claude.present);
        assert_eq!(claude.file_count, 1);
        assert_eq!(claude.logical_bytes, 5);
        assert!(report.providers[1..].iter().all(|entry| !entry.present));
    }

    #[test]
    fn rejects_a_vault_inside_a_provider_root() {
        let temp = TempDir::new().unwrap();
        let claude = temp.path().join("claude");
        let target = claude.join("vault");
        fs::create_dir_all(&target).unwrap();
        let mut config = sample_config(target);
        config.sources.claude_home = Some(claude);
        config.sources.codex_home = Some(temp.path().join("missing-codex"));
        config.sources.grok_home = Some(temp.path().join("missing-grok"));
        config.sources.kimi_home = Some(temp.path().join("missing-kimi"));
        config.sources.opencode_share = Some(temp.path().join("missing-opencode-share"));
        config.sources.opencode_state = Some(temp.path().join("missing-opencode-state"));

        let report = inspect(Path::new("/config.toml"), &config);
        assert!(!report.healthy);
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.contains("overlaps provider root"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn overlap_check_resolves_symlinked_parent_aliases() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let real = temp.path().join("real");
        let alias = temp.path().join("alias");
        fs::create_dir_all(real.join("provider/vault")).unwrap();
        symlink(&real, &alias).unwrap();

        assert!(paths_overlap(
            &real.join("provider/vault"),
            &alias.join("provider")
        ));
        assert!(paths_overlap(
            &real.join("provider/future-vault"),
            &alias.join("provider")
        ));
    }

    fn sample_config(target: PathBuf) -> Config {
        let state_path = target.with_extension("state");
        fs::create_dir_all(&state_path).unwrap();
        Config {
            format_version: 1,
            vault: VaultConfig {
                id: Uuid::nil(),
                created_at: DateTime::from_timestamp(0, 0).unwrap(),
                state_path,
            },
            target: TargetConfig::Filesystem { path: target },
            archive: ArchiveConfig {
                chunk_size: 1024,
                compression_level: 3,
            },
            encryption: EncryptionConfig {
                mode: EncryptionMode::None,
                recipient: None,
                identity_file: None,
            },
            sources: SourceOverrides::default(),
        }
    }
}
