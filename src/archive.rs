use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use filetime::FileTime;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::config::{self, Config, TargetConfig, create_private_directory};
use crate::doctor;
use crate::manifest::{
    ArchiveDescriptor, ChunkRecord, FileRecord, MANIFEST_FORMAT_VERSION, Manifest, ProviderSummary,
    SnapshotStats, validate_commit_message, validate_logical_path,
};
use crate::providers::Provider;
use crate::source::{PreparedFile, prepare_files};
use crate::vault::Vault;

const MAX_FILE_BATCH_CHUNKS: usize = 4;

struct ArchivedFile {
    record: FileRecord,
    unique_objects: HashSet<String>,
    new_objects: HashMap<String, u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BackupReport {
    pub snapshot_id: String,
    pub files: u64,
    pub logical_bytes: u64,
    pub chunk_references: u64,
    pub unique_objects: u64,
    pub new_objects: u64,
    pub new_stored_bytes: u64,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SnapshotInfo {
    pub snapshot_id: String,
    pub parent: Option<String>,
    pub message: Option<String>,
    pub created_at: chrono::DateTime<Utc>,
    pub hostname: String,
    pub files: u64,
    pub providers: Vec<Provider>,
    pub logical_bytes: u64,
    pub unique_raw_bytes: u64,
    pub duplicate_raw_bytes: u64,
    pub unique_objects: u64,
    pub stored_bytes: u64,
    pub verification: VerificationLevel,
    pub full_verified_at: Option<chrono::DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerificationLevel {
    Quick,
    Full,
}

impl std::fmt::Display for VerificationLevel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Quick => formatter.write_str("quick"),
            Self::Full => formatter.write_str("full"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerifyReport {
    pub snapshot_id: String,
    pub full: bool,
    pub files: u64,
    pub logical_bytes: u64,
    pub unique_objects: u64,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveryReport {
    pub snapshot_id: String,
    pub provider: Option<Provider>,
    pub target: PathBuf,
    pub files: u64,
    pub logical_bytes: u64,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiffReport {
    pub from: String,
    pub to: String,
    pub files_added: u64,
    pub files_modified: u64,
    pub files_removed: u64,
    pub bytes_from: u64,
    pub bytes_to: u64,
    pub providers: Vec<ProviderDiff>,
    pub changes: Vec<FileChange>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderDiff {
    pub provider: Provider,
    pub files_added: u64,
    pub files_modified: u64,
    pub files_removed: u64,
    pub bytes_from: u64,
    pub bytes_to: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileChangeKind {
    Added,
    Modified,
    Removed,
}

impl std::fmt::Display for FileChangeKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Added => "A",
            Self::Modified => "M",
            Self::Removed => "D",
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileChange {
    pub kind: FileChangeKind,
    pub provider: Provider,
    pub logical_path: String,
    pub old_size: Option<u64>,
    pub new_size: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CloneReport {
    pub destination: PathBuf,
    pub config: PathBuf,
    pub repository_objects: u64,
    pub stored_bytes: u64,
    pub head: String,
    pub duration_ms: u64,
}

pub fn backup(config_path: &Path, config: &Config, message: Option<&str>) -> Result<BackupReport> {
    let started = Instant::now();
    if let Some(message) = message {
        validate_commit_message(message)?;
    }
    let diagnosis = doctor::inspect(config_path, config);
    if !diagnosis.healthy {
        bail!("commit preflight failed: {}", diagnosis.errors.join("; "));
    }

    let vault = Vault::open(config)?;
    let _lock = vault.acquire_backup_lock()?;
    let parent = vault.latest_snapshot_id()?;
    let staging = vault.staging_directory()?;
    let prepared = prepare_files(config, staging.path())?;
    if prepared.is_empty() {
        bail!("no provider files were discovered; refusing to publish an empty recovery point");
    }

    let created_at = Utc::now();
    let snapshot_id = format!(
        "{}-{}",
        created_at.format("%Y%m%dT%H%M%S%.3fZ"),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let archived = prepared
        .par_iter()
        .map(|prepared_file| {
            archive_file(
                &vault,
                config.archive.chunk_size as usize,
                config.archive.compression_level,
                prepared_file,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let mut files = Vec::with_capacity(archived.len());
    let mut unique_objects = HashSet::new();
    let mut new_objects = HashSet::new();
    let mut new_stored_bytes = 0_u64;
    for archived_file in archived {
        unique_objects.extend(archived_file.unique_objects);
        for (id, stored_size) in archived_file.new_objects {
            if new_objects.insert(id) {
                new_stored_bytes = new_stored_bytes
                    .checked_add(stored_size)
                    .context("stored byte count overflow")?;
            }
        }
        files.push(archived_file.record);
    }

    let logical_bytes = files.iter().map(|file| file.size).sum();
    let chunk_references = files.iter().map(|file| file.chunks.len() as u64).sum();
    let providers = summarize_providers(&files);
    let stats = SnapshotStats {
        files: files.len() as u64,
        logical_bytes,
        chunk_references,
        unique_objects: unique_objects.len() as u64,
        new_objects: new_objects.len() as u64,
        new_stored_bytes,
    };
    let manifest = Manifest {
        format_version: MANIFEST_FORMAT_VERSION,
        vault_id: config.vault.id,
        snapshot_id: snapshot_id.clone(),
        parent,
        message: message.map(str::to_string),
        created_at,
        hostname: gethostname::gethostname().to_string_lossy().into_owned(),
        archive: ArchiveDescriptor {
            chunk_algorithm: "fixed".to_string(),
            chunk_size: config.archive.chunk_size,
            compression: "zstd".to_string(),
            compression_level: config.archive.compression_level,
            encryption: config.encryption.mode.to_string(),
        },
        providers,
        files,
        stats: stats.clone(),
    };
    manifest.validate(config.vault.id)?;
    if !new_objects.is_empty() {
        vault.refresh_object_metadata()?;
    }
    verify_object_presence(&vault, &manifest)?;
    vault.publish_manifest(&manifest)?;

    Ok(BackupReport {
        snapshot_id,
        files: stats.files,
        logical_bytes: stats.logical_bytes,
        chunk_references: stats.chunk_references,
        unique_objects: stats.unique_objects,
        new_objects: stats.new_objects,
        new_stored_bytes: stats.new_stored_bytes,
        duration_ms: elapsed_millis(started),
    })
}

pub fn snapshots(config: &Config) -> Result<Vec<SnapshotInfo>> {
    let vault = Vault::open(config)?;
    let manifests = vault.list_manifests()?;
    let mut snapshots = Vec::with_capacity(manifests.len());
    for manifest in manifests {
        manifest.validate(config.vault.id)?;
        let mut seen = HashSet::new();
        let mut stored_bytes = 0_u64;
        let mut unique_raw_bytes = 0_u64;
        for chunk in manifest.files.iter().flat_map(|file| &file.chunks) {
            if seen.insert(chunk.id.as_str()) {
                stored_bytes = stored_bytes
                    .checked_add(chunk.stored_size)
                    .context("snapshot stored byte count overflow")?;
                unique_raw_bytes = unique_raw_bytes
                    .checked_add(chunk.raw_size)
                    .context("snapshot raw byte count overflow")?;
            }
        }
        let full_verified_at = vault.full_verification_time(&manifest.snapshot_id)?;
        snapshots.push(SnapshotInfo {
            snapshot_id: manifest.snapshot_id,
            parent: manifest.parent,
            message: manifest.message,
            created_at: manifest.created_at,
            hostname: manifest.hostname,
            files: manifest.stats.files,
            providers: manifest
                .providers
                .iter()
                .map(|summary| summary.provider)
                .collect(),
            logical_bytes: manifest.stats.logical_bytes,
            unique_raw_bytes,
            duplicate_raw_bytes: manifest
                .stats
                .logical_bytes
                .checked_sub(unique_raw_bytes)
                .context("snapshot unique bytes exceed logical bytes")?,
            unique_objects: manifest.stats.unique_objects,
            stored_bytes,
            verification: if full_verified_at.is_some() {
                VerificationLevel::Full
            } else {
                VerificationLevel::Quick
            },
            full_verified_at,
        });
    }
    snapshots.sort_by_key(|snapshot| std::cmp::Reverse(snapshot.created_at));
    Ok(snapshots)
}

pub fn verify(config: &Config, reference: &str, full: bool) -> Result<VerifyReport> {
    let started = Instant::now();
    let vault = Vault::open(config)?;
    let manifest = vault.load_manifest(reference)?;
    manifest.validate(config.vault.id)?;

    if full {
        manifest
            .files
            .par_iter()
            .map(|file| verify_file(&vault, file))
            .collect::<Result<Vec<_>>>()?;
        vault.record_full_verification(&manifest.snapshot_id)?;
    } else {
        verify_object_presence(&vault, &manifest)?;
    }

    Ok(VerifyReport {
        snapshot_id: manifest.snapshot_id,
        full,
        files: manifest.stats.files,
        logical_bytes: manifest.stats.logical_bytes,
        unique_objects: manifest.stats.unique_objects,
        duration_ms: elapsed_millis(started),
    })
}

pub fn recover(
    config: &Config,
    reference: &str,
    target: &Path,
    provider: Option<Provider>,
) -> Result<RecoveryReport> {
    let started = Instant::now();
    let vault = Vault::open(config)?;
    let manifest = vault.load_manifest(reference)?;
    manifest.validate(config.vault.id)?;
    let selected: Vec<_> = manifest
        .files
        .iter()
        .filter(|record| provider.is_none_or(|provider| record.provider == provider))
        .collect();
    if selected.is_empty() {
        if let Some(provider) = provider {
            bail!(
                "recovery point {} contains no {} files",
                manifest.snapshot_id,
                provider
            );
        }
        bail!("recovery point {} contains no files", manifest.snapshot_id);
    }
    let files = u64::try_from(selected.len()).context("selected file count exceeds u64")?;
    let logical_bytes = selected.iter().try_fold(0_u64, |total, record| {
        total
            .checked_add(record.size)
            .context("selected logical bytes exceed u64")
    })?;
    prepare_recovery_target(vault.filesystem_root(), vault.state_root(), target)?;

    let marker = target.join(".akeep-recovery-incomplete");
    create_private_new_file(&marker)?;
    let recovery_result = recover_files(&vault, &selected, target);
    if recovery_result.is_ok() {
        fs::remove_file(&marker)
            .with_context(|| format!("failed to remove recovery marker {}", marker.display()))?;
    }
    recovery_result?;
    if provider.is_none() {
        vault.record_full_verification(&manifest.snapshot_id)?;
    }

    Ok(RecoveryReport {
        snapshot_id: manifest.snapshot_id,
        provider,
        target: fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf()),
        files,
        logical_bytes,
        duration_ms: elapsed_millis(started),
    })
}

pub fn diff(config: &Config, from: &str, to: &str) -> Result<DiffReport> {
    let vault = Vault::open(config)?;
    let before = vault.load_manifest(from)?;
    let after = vault.load_manifest(to)?;
    before.validate(config.vault.id)?;
    after.validate(config.vault.id)?;

    let before_files = before
        .files
        .iter()
        .map(|file| (file.logical_path.as_str(), file))
        .collect::<BTreeMap<_, _>>();
    let after_files = after
        .files
        .iter()
        .map(|file| (file.logical_path.as_str(), file))
        .collect::<BTreeMap<_, _>>();
    let paths = before_files
        .keys()
        .chain(after_files.keys())
        .copied()
        .collect::<BTreeSet<_>>();

    let mut changes = Vec::new();
    for path in paths {
        let change = match (before_files.get(path), after_files.get(path)) {
            (None, Some(file)) => Some(FileChange {
                kind: FileChangeKind::Added,
                provider: file.provider,
                logical_path: path.to_string(),
                old_size: None,
                new_size: Some(file.size),
            }),
            (Some(file), None) => Some(FileChange {
                kind: FileChangeKind::Removed,
                provider: file.provider,
                logical_path: path.to_string(),
                old_size: Some(file.size),
                new_size: None,
            }),
            (Some(old), Some(new)) if old != new => Some(FileChange {
                kind: FileChangeKind::Modified,
                provider: new.provider,
                logical_path: path.to_string(),
                old_size: Some(old.size),
                new_size: Some(new.size),
            }),
            _ => None,
        };
        if let Some(change) = change {
            changes.push(change);
        }
    }

    let mut provider_diffs = BTreeMap::<Provider, ProviderDiff>::new();
    for file in &before.files {
        let summary = provider_diffs
            .entry(file.provider)
            .or_insert_with(|| empty_provider_diff(file.provider));
        summary.bytes_from = summary
            .bytes_from
            .checked_add(file.size)
            .context("provider byte count overflow")?;
    }
    for file in &after.files {
        let summary = provider_diffs
            .entry(file.provider)
            .or_insert_with(|| empty_provider_diff(file.provider));
        summary.bytes_to = summary
            .bytes_to
            .checked_add(file.size)
            .context("provider byte count overflow")?;
    }
    for change in &changes {
        let summary = provider_diffs
            .entry(change.provider)
            .or_insert_with(|| empty_provider_diff(change.provider));
        match change.kind {
            FileChangeKind::Added => summary.files_added += 1,
            FileChangeKind::Modified => summary.files_modified += 1,
            FileChangeKind::Removed => summary.files_removed += 1,
        }
    }

    Ok(DiffReport {
        from: before.snapshot_id,
        to: after.snapshot_id,
        files_added: changes
            .iter()
            .filter(|change| change.kind == FileChangeKind::Added)
            .count() as u64,
        files_modified: changes
            .iter()
            .filter(|change| change.kind == FileChangeKind::Modified)
            .count() as u64,
        files_removed: changes
            .iter()
            .filter(|change| change.kind == FileChangeKind::Removed)
            .count() as u64,
        bytes_from: before.stats.logical_bytes,
        bytes_to: after.stats.logical_bytes,
        providers: provider_diffs.into_values().collect(),
        changes,
    })
}

pub fn clone_repository(config: &Config, destination: &Path) -> Result<CloneReport> {
    let started = Instant::now();
    let source = Vault::open(config)?;
    let head = source
        .latest_snapshot_id()?
        .context("repository has no commits to clone")?;
    prepare_clone_destination(source.filesystem_root(), source.state_root(), destination)?;

    let marker = destination.join(".akeep-clone-incomplete");
    drop(create_private_new_file(&marker)?);
    let repository = destination.join("repository");
    let state = destination.join("state");
    create_private_directory(&repository)?;
    create_private_directory(&state)?;
    let repository = fs::canonicalize(&repository)
        .with_context(|| format!("failed to resolve {}", repository.display()))?;
    let state = fs::canonicalize(&state)
        .with_context(|| format!("failed to resolve {}", state.display()))?;

    let mut cloned_config = config.clone();
    cloned_config.target = TargetConfig::Filesystem {
        path: repository.clone(),
    };
    cloned_config.vault.state_path = state;
    cloned_config.validate()?;

    let copied = source.copy_repository_to(&repository)?;
    let config_path = destination.join("config.toml");
    config::write_new(&config_path, &cloned_config)?;

    let clone = Vault::open(&cloned_config)?;
    let manifest = clone.load_manifest("HEAD")?;
    manifest.validate(cloned_config.vault.id)?;
    if manifest.snapshot_id != head {
        bail!("cloned HEAD is {}, expected {}", manifest.snapshot_id, head);
    }
    verify_object_presence(&clone, &manifest)?;
    fs::remove_file(&marker)
        .with_context(|| format!("failed to remove clone marker {}", marker.display()))?;

    Ok(CloneReport {
        destination: fs::canonicalize(destination).unwrap_or_else(|_| destination.to_path_buf()),
        config: fs::canonicalize(&config_path).unwrap_or(config_path),
        repository_objects: copied.objects,
        stored_bytes: copied.stored_bytes,
        head,
        duration_ms: elapsed_millis(started),
    })
}

fn empty_provider_diff(provider: Provider) -> ProviderDiff {
    ProviderDiff {
        provider,
        files_added: 0,
        files_modified: 0,
        files_removed: 0,
        bytes_from: 0,
        bytes_to: 0,
    }
}

fn archive_file(
    vault: &Vault,
    chunk_size: usize,
    compression_level: i32,
    prepared: &PreparedFile,
) -> Result<ArchivedFile> {
    let file = File::open(&prepared.read_path)
        .with_context(|| format!("failed to read source {}", prepared.read_path.display()))?;
    let mut reader = BufReader::new(file);
    let mut file_hasher = blake3::Hasher::new();
    let mut chunks = Vec::new();
    let mut unique_objects = HashSet::new();
    let mut new_objects = HashMap::new();
    let mut size = 0_u64;
    let mut buffer = vec![0_u8; chunk_size];
    let batch_chunks = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(4)
        .clamp(1, MAX_FILE_BATCH_CHUNKS);

    loop {
        let mut batch = Vec::with_capacity(batch_chunks);
        for _ in 0..batch_chunks {
            let read = read_full_chunk(&mut reader, &mut buffer)?;
            if read == 0 {
                break;
            }
            let raw = buffer[..read].to_vec();
            file_hasher.update(&raw);
            size = size
                .checked_add(read as u64)
                .context("source file size overflow")?;
            batch.push(raw);
        }
        if batch.is_empty() {
            break;
        }

        let mut first_index_by_id = HashMap::new();
        let batch_ids = batch
            .iter()
            .enumerate()
            .map(|(index, raw)| {
                let id = blake3::hash(raw).to_hex().to_string();
                first_index_by_id.entry(id.clone()).or_insert(index);
                id
            })
            .collect::<Vec<_>>();
        let unique_indices = first_index_by_id.values().copied().collect::<Vec<_>>();
        let stored_unique = unique_indices
            .par_iter()
            .map(|index| {
                vault
                    .store_chunk(&batch[*index], compression_level)
                    .map(|stored| (*index, stored))
            })
            .collect::<Result<Vec<_>>>()?;
        let stored_by_index = stored_unique.into_iter().collect::<HashMap<_, _>>();

        for (index, raw) in batch.iter().enumerate() {
            let first_index = *first_index_by_id
                .get(&batch_ids[index])
                .context("internal chunk batch index is missing")?;
            let stored = stored_by_index
                .get(&first_index)
                .context("internal stored chunk result is missing")?
                .clone();
            unique_objects.insert(stored.id.clone());
            if stored.is_new {
                new_objects
                    .entry(stored.id.clone())
                    .or_insert(stored.stored_size);
            }
            chunks.push(ChunkRecord {
                id: stored.id,
                raw_size: raw.len() as u64,
                stored_size: stored.stored_size,
            });
        }
    }

    Ok(ArchivedFile {
        record: FileRecord {
            provider: prepared.provider,
            logical_path: prepared.logical_path.clone(),
            kind: prepared.kind,
            size,
            blake3: file_hasher.finalize().to_hex().to_string(),
            modified_unix_seconds: prepared.modified_unix_seconds,
            modified_subsec_nanos: prepared.modified_subsec_nanos,
            unix_mode: prepared.unix_mode,
            chunks,
        },
        unique_objects,
        new_objects,
    })
}

fn read_full_chunk(reader: &mut impl Read, buffer: &mut [u8]) -> Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        let read = reader
            .read(&mut buffer[filled..])
            .context("failed while reading source file")?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

fn summarize_providers(files: &[FileRecord]) -> Vec<ProviderSummary> {
    let mut summaries: BTreeMap<Provider, (u64, u64)> = BTreeMap::new();
    for file in files {
        let summary = summaries.entry(file.provider).or_default();
        summary.0 += 1;
        summary.1 += file.size;
    }
    summaries
        .into_iter()
        .map(|(provider, (files, logical_bytes))| ProviderSummary {
            provider,
            adapter_version: Provider::ADAPTER_VERSION,
            files,
            logical_bytes,
        })
        .collect()
}

fn verify_object_presence(vault: &Vault, manifest: &Manifest) -> Result<()> {
    let mut seen = HashSet::new();
    for chunk in manifest.files.iter().flat_map(|file| &file.chunks) {
        if !seen.insert(&chunk.id) {
            continue;
        }
        let metadata = vault.object_metadata(&chunk.id)?;
        if metadata.size != chunk.stored_size {
            bail!(
                "stored size mismatch for object {}: got {}, expected {}",
                chunk.id,
                metadata.size,
                chunk.stored_size
            );
        }
    }
    Ok(())
}

fn verify_file(vault: &Vault, file: &FileRecord) -> Result<()> {
    validate_logical_path(&file.logical_path)?;
    let mut file_hasher = blake3::Hasher::new();
    let mut file_size = 0_u64;
    for chunk_batch in file.chunks.chunks(MAX_FILE_BATCH_CHUNKS) {
        for raw in read_verified_chunks(vault, chunk_batch)? {
            file_hasher.update(&raw);
            file_size += raw.len() as u64;
        }
    }
    if file_size != file.size {
        bail!(
            "file size mismatch for {}: got {}, expected {}",
            file.logical_path,
            file_size,
            file.size
        );
    }
    let hash = file_hasher.finalize().to_hex().to_string();
    if hash != file.blake3 {
        bail!(
            "file hash mismatch for {}: got {}, expected {}",
            file.logical_path,
            hash,
            file.blake3
        );
    }
    Ok(())
}

fn verify_chunk(chunk: &ChunkRecord, raw: &[u8]) -> Result<()> {
    if raw.len() as u64 != chunk.raw_size {
        bail!(
            "raw size mismatch for object {}: got {}, expected {}",
            chunk.id,
            raw.len(),
            chunk.raw_size
        );
    }
    let hash = blake3::hash(raw).to_hex().to_string();
    if hash != chunk.id {
        bail!(
            "content hash mismatch for object {}: decoded as {}",
            chunk.id,
            hash
        );
    }
    Ok(())
}

fn prepare_recovery_target(
    vault_root: Option<&Path>,
    state_root: &Path,
    target: &Path,
) -> Result<()> {
    let resolved_target = resolve_future_path(target)?;
    for protected in vault_root.into_iter().chain(std::iter::once(state_root)) {
        let resolved = fs::canonicalize(protected)
            .with_context(|| format!("failed to resolve {}", protected.display()))?;
        if paths_overlap(&resolved, &resolved_target) {
            bail!(
                "recovery target {} overlaps vault/state path {}; choose a separate directory",
                target.display(),
                protected.display()
            );
        }
    }
    if target.exists() {
        let metadata = fs::symlink_metadata(target)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!(
                "recovery target must be a real directory: {}",
                target.display()
            );
        }
        if fs::read_dir(target)?.next().is_some() {
            bail!("recovery target is not empty: {}", target.display());
        }
    } else {
        create_private_directory(target)?;
    }
    Ok(())
}

fn prepare_clone_destination(
    vault_root: Option<&Path>,
    state_root: &Path,
    destination: &Path,
) -> Result<()> {
    let resolved_destination = resolve_future_path(destination)?;
    for protected in vault_root.into_iter().chain(std::iter::once(state_root)) {
        let resolved = fs::canonicalize(protected)
            .with_context(|| format!("failed to resolve {}", protected.display()))?;
        if paths_overlap(&resolved, &resolved_destination) {
            bail!(
                "clone destination {} overlaps repository/state path {}; choose a separate directory",
                destination.display(),
                protected.display()
            );
        }
    }
    if destination.exists() {
        bail!(
            "clone destination already exists; choose a new directory: {}",
            destination.display()
        );
    }
    create_private_directory(destination)
}

fn resolve_future_path(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return fs::canonicalize(path)
            .with_context(|| format!("failed to resolve {}", path.display()));
    }

    let mut missing = Vec::new();
    let mut ancestor = path;
    while !ancestor.exists() {
        let name = ancestor
            .file_name()
            .with_context(|| format!("could not resolve future path {}", path.display()))?;
        missing.push(name.to_os_string());
        ancestor = ancestor
            .parent()
            .with_context(|| format!("could not resolve future path {}", path.display()))?;
    }
    let mut resolved = fs::canonicalize(ancestor)
        .with_context(|| format!("failed to resolve ancestor {}", ancestor.display()))?;
    for component in missing.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn recover_files(vault: &Vault, records: &[&FileRecord], target: &Path) -> Result<()> {
    records
        .par_iter()
        .map(|record| recover_file(vault, record, target))
        .collect::<Result<Vec<_>>>()?;
    Ok(())
}

fn recover_file(vault: &Vault, record: &FileRecord, target: &Path) -> Result<()> {
    validate_logical_path(&record.logical_path)?;
    let output = target.join(&record.logical_path);
    let parent = output
        .parent()
        .with_context(|| format!("recovery path {} has no parent", output.display()))?;
    create_private_directory(parent)?;
    let mut file = create_private_new_file(&output)?;
    let mut file_hasher = blake3::Hasher::new();
    let mut file_size = 0_u64;
    for chunk_batch in record.chunks.chunks(MAX_FILE_BATCH_CHUNKS) {
        for raw in read_verified_chunks(vault, chunk_batch)? {
            file.write_all(&raw)
                .with_context(|| format!("failed to recover {}", output.display()))?;
            file_hasher.update(&raw);
            file_size += raw.len() as u64;
        }
    }
    file.sync_all()
        .with_context(|| format!("failed to sync recovered file {}", output.display()))?;
    if file_size != record.size || file_hasher.finalize().to_hex().as_str() != record.blake3 {
        bail!("recovered file failed verification: {}", output.display());
    }
    apply_metadata(&output, record)
}

fn read_verified_chunks(vault: &Vault, chunks: &[ChunkRecord]) -> Result<Vec<Vec<u8>>> {
    chunks
        .par_iter()
        .map(|chunk| {
            let raw = vault.read_chunk(&chunk.id)?;
            verify_chunk(chunk, &raw)?;
            Ok(raw)
        })
        .collect()
}

fn create_private_new_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("refusing to overwrite {}", path.display()))
}

fn apply_metadata(path: &Path, record: &FileRecord) -> Result<()> {
    #[cfg(unix)]
    if let Some(mode) = record.unix_mode {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .with_context(|| format!("failed to restore permissions on {}", path.display()))?;
    }
    if let (Some(seconds), Some(nanos)) =
        (record.modified_unix_seconds, record.modified_subsec_nanos)
    {
        let modified = FileTime::from_unix_time(seconds, nanos);
        filetime::set_file_mtime(path, modified)
            .with_context(|| format!("failed to restore timestamp on {}", path.display()))?;
    }
    Ok(())
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn fills_chunks_across_short_reads() {
        struct ShortReader(Cursor<Vec<u8>>);
        impl Read for ShortReader {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                let length = buffer.len().min(2);
                self.0.read(&mut buffer[..length])
            }
        }

        let mut reader = ShortReader(Cursor::new(b"abcdef".to_vec()));
        let mut buffer = [0_u8; 5];
        assert_eq!(read_full_chunk(&mut reader, &mut buffer).unwrap(), 5);
        assert_eq!(&buffer, b"abcde");
        assert_eq!(read_full_chunk(&mut reader, &mut buffer).unwrap(), 1);
        assert_eq!(buffer[0], b'f');
    }
}
