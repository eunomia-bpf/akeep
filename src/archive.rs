use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use filetime::FileTime;
use serde::{Deserialize, Serialize};

use crate::config::{Config, create_private_directory};
use crate::doctor;
use crate::manifest::{
    ArchiveDescriptor, ChunkRecord, FileRecord, MANIFEST_FORMAT_VERSION, Manifest, ProviderSummary,
    SnapshotStats, validate_logical_path,
};
use crate::providers::Provider;
use crate::source::{PreparedFile, prepare_files};
use crate::vault::Vault;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BackupReport {
    pub snapshot_id: String,
    pub files: u64,
    pub logical_bytes: u64,
    pub chunk_references: u64,
    pub unique_objects: u64,
    pub new_objects: u64,
    pub new_stored_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SnapshotInfo {
    pub snapshot_id: String,
    pub created_at: chrono::DateTime<Utc>,
    pub hostname: String,
    pub files: u64,
    pub logical_bytes: u64,
    pub unique_objects: u64,
    pub stored_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerifyReport {
    pub snapshot_id: String,
    pub full: bool,
    pub files: u64,
    pub logical_bytes: u64,
    pub unique_objects: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveryReport {
    pub snapshot_id: String,
    pub target: PathBuf,
    pub files: u64,
    pub logical_bytes: u64,
}

pub fn backup(config_path: &Path, config: &Config) -> Result<BackupReport> {
    let diagnosis = doctor::inspect(config_path, config);
    if !diagnosis.healthy {
        bail!("backup preflight failed: {}", diagnosis.errors.join("; "));
    }

    let vault = Vault::open(config)?;
    let _lock = vault.acquire_backup_lock()?;
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
    let mut files = Vec::with_capacity(prepared.len());
    let mut unique_objects = HashSet::new();
    let mut new_objects = HashSet::new();
    let mut new_stored_bytes = 0_u64;

    for prepared_file in &prepared {
        let record = archive_file(
            &vault,
            config.archive.chunk_size as usize,
            config.archive.compression_level,
            prepared_file,
            &mut unique_objects,
            &mut new_objects,
            &mut new_stored_bytes,
        )?;
        files.push(record);
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
    })
}

pub fn snapshots(config: &Config) -> Result<Vec<SnapshotInfo>> {
    let vault = Vault::open(config)?;
    let manifests = vault.list_manifests()?;
    let mut snapshots = Vec::with_capacity(manifests.len());
    for manifest in manifests {
        manifest.validate(config.vault.id)?;
        let mut seen = HashSet::new();
        let stored_bytes = manifest
            .files
            .iter()
            .flat_map(|file| &file.chunks)
            .filter(|chunk| seen.insert(chunk.id.as_str()))
            .map(|chunk| chunk.stored_size)
            .sum();
        snapshots.push(SnapshotInfo {
            snapshot_id: manifest.snapshot_id,
            created_at: manifest.created_at,
            hostname: manifest.hostname,
            files: manifest.stats.files,
            logical_bytes: manifest.stats.logical_bytes,
            unique_objects: manifest.stats.unique_objects,
            stored_bytes,
        });
    }
    snapshots.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    Ok(snapshots)
}

pub fn verify(config: &Config, reference: &str, full: bool) -> Result<VerifyReport> {
    let vault = Vault::open(config)?;
    let manifest = vault.load_manifest(reference)?;
    manifest.validate(config.vault.id)?;

    if full {
        for file in &manifest.files {
            verify_file(&vault, file)?;
        }
    } else {
        verify_object_presence(&vault, &manifest)?;
    }

    Ok(VerifyReport {
        snapshot_id: manifest.snapshot_id,
        full,
        files: manifest.stats.files,
        logical_bytes: manifest.stats.logical_bytes,
        unique_objects: manifest.stats.unique_objects,
    })
}

pub fn recover(config: &Config, reference: &str, target: &Path) -> Result<RecoveryReport> {
    let vault = Vault::open(config)?;
    let manifest = vault.load_manifest(reference)?;
    manifest.validate(config.vault.id)?;
    prepare_recovery_target(vault.root(), target)?;

    let marker = target.join(".akeep-recovery-incomplete");
    create_private_new_file(&marker)?;
    let recovery_result = recover_files(&vault, &manifest, target);
    if recovery_result.is_ok() {
        fs::remove_file(&marker)
            .with_context(|| format!("failed to remove recovery marker {}", marker.display()))?;
    }
    recovery_result?;

    Ok(RecoveryReport {
        snapshot_id: manifest.snapshot_id,
        target: fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf()),
        files: manifest.stats.files,
        logical_bytes: manifest.stats.logical_bytes,
    })
}

fn archive_file(
    vault: &Vault,
    chunk_size: usize,
    compression_level: i32,
    prepared: &PreparedFile,
    unique_objects: &mut HashSet<String>,
    new_objects: &mut HashSet<String>,
    new_stored_bytes: &mut u64,
) -> Result<FileRecord> {
    let file = File::open(&prepared.read_path)
        .with_context(|| format!("failed to read source {}", prepared.read_path.display()))?;
    let mut reader = BufReader::new(file);
    let mut file_hasher = blake3::Hasher::new();
    let mut chunks = Vec::new();
    let mut size = 0_u64;
    let mut buffer = vec![0_u8; chunk_size];

    loop {
        let read = read_full_chunk(&mut reader, &mut buffer)?;
        if read == 0 {
            break;
        }
        let raw = &buffer[..read];
        file_hasher.update(raw);
        size = size
            .checked_add(read as u64)
            .context("source file size overflow")?;
        let stored = vault.store_chunk(raw, compression_level)?;
        unique_objects.insert(stored.id.clone());
        if stored.is_new && new_objects.insert(stored.id.clone()) {
            *new_stored_bytes = new_stored_bytes
                .checked_add(stored.stored_size)
                .context("stored byte count overflow")?;
        }
        chunks.push(ChunkRecord {
            id: stored.id,
            raw_size: read as u64,
            stored_size: stored.stored_size,
        });
    }

    Ok(FileRecord {
        provider: prepared.provider,
        logical_path: prepared.logical_path.clone(),
        kind: prepared.kind,
        size,
        blake3: file_hasher.finalize().to_hex().to_string(),
        modified_unix_seconds: prepared.modified_unix_seconds,
        modified_subsec_nanos: prepared.modified_subsec_nanos,
        unix_mode: prepared.unix_mode,
        chunks,
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
        if metadata.len() != chunk.stored_size {
            bail!(
                "stored size mismatch for object {}: got {}, expected {}",
                chunk.id,
                metadata.len(),
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
    for chunk in &file.chunks {
        let raw = vault.read_chunk(&chunk.id)?;
        verify_chunk(chunk, &raw)?;
        file_hasher.update(&raw);
        file_size += raw.len() as u64;
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

fn prepare_recovery_target(vault_root: &Path, target: &Path) -> Result<()> {
    let resolved_vault = fs::canonicalize(vault_root)
        .with_context(|| format!("failed to resolve vault {}", vault_root.display()))?;
    let resolved_target = resolve_future_path(target)?;
    if paths_overlap(&resolved_vault, &resolved_target) {
        bail!(
            "recovery target {} overlaps vault {}; choose a separate directory",
            target.display(),
            vault_root.display()
        );
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

fn recover_files(vault: &Vault, manifest: &Manifest, target: &Path) -> Result<()> {
    for record in &manifest.files {
        validate_logical_path(&record.logical_path)?;
        let output = target.join(&record.logical_path);
        let parent = output
            .parent()
            .with_context(|| format!("recovery path {} has no parent", output.display()))?;
        create_private_directory(parent)?;
        let mut file = create_private_new_file(&output)?;
        let mut file_hasher = blake3::Hasher::new();
        let mut file_size = 0_u64;
        for chunk in &record.chunks {
            let raw = vault.read_chunk(&chunk.id)?;
            verify_chunk(chunk, &raw)?;
            file.write_all(&raw)
                .with_context(|| format!("failed to recover {}", output.display()))?;
            file_hasher.update(&raw);
            file_size += raw.len() as u64;
        }
        file.sync_all()
            .with_context(|| format!("failed to sync recovered file {}", output.display()))?;
        if file_size != record.size || file_hasher.finalize().to_hex().as_str() != record.blake3 {
            bail!("recovered file failed verification: {}", output.display());
        }
        apply_metadata(&output, record)?;
    }
    Ok(())
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
