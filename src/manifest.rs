use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path};

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::providers::Provider;

pub const MANIFEST_FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format_version: u32,
    pub vault_id: Uuid,
    pub snapshot_id: String,
    pub created_at: DateTime<Utc>,
    pub hostname: String,
    pub archive: ArchiveDescriptor,
    pub providers: Vec<ProviderSummary>,
    pub files: Vec<FileRecord>,
    pub stats: SnapshotStats,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveDescriptor {
    pub chunk_algorithm: String,
    pub chunk_size: u64,
    pub compression: String,
    pub compression_level: i32,
    pub encryption: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSummary {
    pub provider: Provider,
    #[serde(default = "default_adapter_version")]
    pub adapter_version: u32,
    pub files: u64,
    pub logical_bytes: u64,
}

fn default_adapter_version() -> u32 {
    Provider::ADAPTER_VERSION
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArchivedFileKind {
    Regular,
    SqliteSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileRecord {
    pub provider: Provider,
    pub logical_path: String,
    pub kind: ArchivedFileKind,
    pub size: u64,
    pub blake3: String,
    pub modified_unix_seconds: Option<i64>,
    pub modified_subsec_nanos: Option<u32>,
    pub unix_mode: Option<u32>,
    pub chunks: Vec<ChunkRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkRecord {
    pub id: String,
    pub raw_size: u64,
    pub stored_size: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotStats {
    pub files: u64,
    pub logical_bytes: u64,
    pub chunk_references: u64,
    pub unique_objects: u64,
    pub new_objects: u64,
    pub new_stored_bytes: u64,
}

impl Manifest {
    pub fn validate(&self, expected_vault_id: Uuid) -> Result<()> {
        if self.format_version != MANIFEST_FORMAT_VERSION {
            bail!(
                "unsupported manifest format version {} (expected {})",
                self.format_version,
                MANIFEST_FORMAT_VERSION
            );
        }
        if self.vault_id != expected_vault_id {
            bail!(
                "manifest belongs to vault {}, not {}",
                self.vault_id,
                expected_vault_id
            );
        }
        validate_snapshot_id(&self.snapshot_id)?;
        if self.archive.chunk_algorithm != "fixed" {
            bail!(
                "unsupported chunk algorithm {}",
                self.archive.chunk_algorithm
            );
        }
        if self.archive.chunk_size == 0 {
            bail!("manifest chunk size must be greater than zero");
        }
        if self.archive.compression != "zstd" {
            bail!("unsupported compression {}", self.archive.compression);
        }
        if !matches!(self.archive.encryption.as_str(), "none" | "age") {
            bail!("unsupported encryption {}", self.archive.encryption);
        }

        let mut logical_paths = HashSet::new();
        let mut unique_objects = HashSet::new();
        let mut provider_totals: BTreeMap<Provider, (u64, u64)> = BTreeMap::new();
        let mut logical_bytes = 0_u64;
        let mut chunk_references = 0_u64;
        for file in &self.files {
            validate_logical_path(&file.logical_path)?;
            let provider_prefix = format!("{}/", file.provider.id());
            if !file.logical_path.starts_with(&provider_prefix) {
                bail!(
                    "logical path {} does not match provider {}",
                    file.logical_path,
                    file.provider
                );
            }
            if !logical_paths.insert(&file.logical_path) {
                bail!("duplicate logical path {}", file.logical_path);
            }
            if file.blake3.len() != 64 || !file.blake3.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                bail!("invalid file hash for {}", file.logical_path);
            }
            if file.blake3.bytes().any(|byte| byte.is_ascii_uppercase()) {
                bail!("file hash must be lowercase for {}", file.logical_path);
            }
            if file
                .modified_subsec_nanos
                .is_some_and(|nanos| nanos >= 1_000_000_000)
            {
                bail!("invalid modification time for {}", file.logical_path);
            }
            if file.unix_mode.is_some_and(|mode| mode > 0o7777) {
                bail!("invalid Unix mode for {}", file.logical_path);
            }
            let chunk_bytes = file.chunks.iter().try_fold(0_u64, |total, chunk| {
                total
                    .checked_add(chunk.raw_size)
                    .ok_or_else(|| anyhow::anyhow!("chunk byte count overflow"))
            })?;
            if chunk_bytes != file.size {
                bail!(
                    "chunk sizes for {} total {}, expected {}",
                    file.logical_path,
                    chunk_bytes,
                    file.size
                );
            }
            for chunk in &file.chunks {
                validate_object_id(&chunk.id)?;
                if chunk.raw_size == 0 {
                    bail!("zero-length chunk {} in {}", chunk.id, file.logical_path);
                }
                if chunk.stored_size == 0 {
                    bail!("zero stored size for chunk {}", chunk.id);
                }
                if chunk.raw_size > self.archive.chunk_size {
                    bail!(
                        "chunk {} exceeds configured chunk size {}",
                        chunk.id,
                        self.archive.chunk_size
                    );
                }
                unique_objects.insert(&chunk.id);
                chunk_references = chunk_references
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("chunk reference count overflow"))?;
            }
            logical_bytes = logical_bytes
                .checked_add(file.size)
                .ok_or_else(|| anyhow::anyhow!("manifest logical byte count overflow"))?;
            let provider_total = provider_totals.entry(file.provider).or_default();
            provider_total.0 = provider_total
                .0
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("provider file count overflow"))?;
            provider_total.1 = provider_total
                .1
                .checked_add(file.size)
                .ok_or_else(|| anyhow::anyhow!("provider byte count overflow"))?;
        }

        let declared_providers: BTreeMap<_, _> = self
            .providers
            .iter()
            .map(|summary| {
                if summary.adapter_version == 0 {
                    bail!("provider adapter version must be greater than zero");
                }
                Ok((summary.provider, (summary.files, summary.logical_bytes)))
            })
            .collect::<Result<_>>()?;
        if declared_providers.len() != self.providers.len() {
            bail!("manifest contains duplicate provider summaries");
        }
        if declared_providers != provider_totals {
            bail!("provider summaries do not match file records");
        }

        if self.stats.files != self.files.len() as u64 {
            bail!(
                "manifest stats report {} files, but {} records exist",
                self.stats.files,
                self.files.len()
            );
        }
        if self.stats.logical_bytes != logical_bytes {
            bail!(
                "manifest stats report {} logical bytes, but file records total {}",
                self.stats.logical_bytes,
                logical_bytes
            );
        }
        if self.stats.chunk_references != chunk_references {
            bail!(
                "manifest stats report {} chunk references, but {} exist",
                self.stats.chunk_references,
                chunk_references
            );
        }
        if self.stats.unique_objects != unique_objects.len() as u64 {
            bail!(
                "manifest stats report {} unique objects, but {} exist",
                self.stats.unique_objects,
                unique_objects.len()
            );
        }
        Ok(())
    }
}

pub fn validate_snapshot_id(snapshot_id: &str) -> Result<()> {
    if snapshot_id.is_empty()
        || snapshot_id.len() > 96
        || !snapshot_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("invalid snapshot id {snapshot_id:?}");
    }
    Ok(())
}

pub fn validate_object_id(object_id: &str) -> Result<()> {
    if object_id.len() != 64
        || !object_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        bail!("invalid object id {object_id:?}");
    }
    Ok(())
}

pub fn validate_logical_path(logical_path: &str) -> Result<()> {
    if logical_path.is_empty() || logical_path.contains('\\') {
        bail!("unsafe logical path {logical_path:?}");
    }
    let path = Path::new(logical_path);
    if path.is_absolute() {
        bail!("unsafe absolute logical path {logical_path:?}");
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            bail!("unsafe logical path {logical_path:?}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_traversal() {
        for path in [
            "",
            "/absolute",
            "../escape",
            "safe/../escape",
            r"safe\escape",
        ] {
            assert!(validate_logical_path(path).is_err(), "{path}");
        }
        assert!(validate_logical_path("codex/sessions/session.jsonl").is_ok());
    }

    #[test]
    fn validates_object_ids() {
        assert!(validate_object_id(&"a".repeat(64)).is_ok());
        assert!(validate_object_id(&"A".repeat(64)).is_err());
        assert!(validate_object_id("short").is_err());
    }
}
