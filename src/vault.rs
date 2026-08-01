use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use uuid::Uuid;

use crate::config::{Config, EncryptionMode, create_private_directory};
use crate::crypto::CryptoContext;
use crate::manifest::{
    FileRecord, Manifest, validate_logical_path, validate_object_id, validate_snapshot_id,
};
use crate::storage::{ObjectMetadata, Storage};

const VAULT_METADATA_VERSION: u32 = 1;
const VERIFICATION_RECEIPT_VERSION: u32 = 1;
const CHUNK_LOCK_SHARDS: usize = 64;

pub struct Vault {
    storage: Storage,
    state_root: PathBuf,
    vault_id: Uuid,
    codec: CryptoContext,
    chunk_locks: Vec<std::sync::Mutex<()>>,
}

pub struct VaultLock {
    _file: File,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredChunk {
    pub id: String,
    pub stored_size: u64,
    pub is_new: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RepositoryCopyStats {
    pub objects: u64,
    pub stored_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct VaultMetadata {
    format_version: u32,
    vault_id: Uuid,
    encryption: EncryptionMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recipient: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct VerificationReceipt {
    format_version: u32,
    vault_id: Uuid,
    snapshot_id: String,
    verified_at: DateTime<Utc>,
    level: String,
}

impl Vault {
    pub fn open(config: &Config) -> Result<Self> {
        let vault = Self {
            storage: Storage::from_config(&config.target, &config.vault.state_path)?,
            state_root: config.vault.state_path.clone(),
            vault_id: config.vault.id,
            codec: CryptoContext::from_config(&config.encryption)?,
            chunk_locks: (0..CHUNK_LOCK_SHARDS)
                .map(|_| std::sync::Mutex::new(()))
                .collect(),
        };
        vault.initialize_layout()?;
        vault.initialize_metadata(config)?;
        Ok(vault)
    }

    pub fn filesystem_root(&self) -> Option<&Path> {
        self.storage.filesystem_root()
    }

    pub fn state_root(&self) -> &Path {
        &self.state_root
    }

    pub fn target_description(&self) -> String {
        self.storage.description()
    }

    pub fn acquire_write_lock(&self) -> Result<VaultLock> {
        let lock_path = self.state_root.join("locks").join("backup.lock");
        let file = open_private_rw(&lock_path)?;
        file.try_lock_exclusive().with_context(|| {
            format!("another repository operation holds {}", lock_path.display())
        })?;
        Ok(VaultLock { _file: file })
    }

    pub fn commit_staging_directory(&self) -> Result<tempfile::TempDir> {
        tempfile::Builder::new()
            .prefix("commit-")
            .tempdir_in(self.state_root.join("staging"))
            .context("failed to create private commit staging directory")
    }

    pub fn configure_object_staging(&self, path: &Path) -> Result<()> {
        self.storage.configure_object_staging(path)
    }

    pub fn flush_pending_objects(&self) -> Result<()> {
        self.storage.flush_pending_objects()
    }

    pub fn store_chunk(&self, raw: &[u8], compression_level: i32) -> Result<StoredChunk> {
        let id = blake3::hash(raw).to_hex().to_string();
        let key = self.object_key(&id)?;
        let compressed = zstd::stream::encode_all(Cursor::new(raw), compression_level)
            .context("failed to compress archive chunk")?;
        let encoded = self.codec.encrypt(&compressed)?;
        let stored_size = encoded.len() as u64;
        let shard = usize::from(u8::from_str_radix(&id[..2], 16).context("invalid chunk id")?)
            % self.chunk_locks.len();
        let _chunk_lock = self.chunk_locks[shard]
            .lock()
            .map_err(|_| anyhow::anyhow!("chunk coordination lock is poisoned"))?;

        if let Some(metadata) = self.storage.metadata(&key)? {
            if metadata.size != stored_size {
                bail!(
                    "existing object {id} has size {}, expected {stored_size}",
                    metadata.size
                );
            }
            return Ok(StoredChunk {
                id,
                stored_size: metadata.size,
                is_new: false,
            });
        }

        self.storage.put(&key, &encoded, false)?;

        Ok(StoredChunk {
            id,
            stored_size,
            is_new: true,
        })
    }

    pub fn read_chunk(&self, id: &str) -> Result<Vec<u8>> {
        let key = self.object_key(id)?;
        let encoded = self.storage.get(&key)?;
        let compressed = self.codec.decrypt(&encoded)?;
        zstd::stream::decode_all(Cursor::new(compressed))
            .with_context(|| format!("failed to decompress object {id}"))
    }

    pub fn object_metadata(&self, id: &str) -> Result<ObjectMetadata> {
        let key = self.object_key(id)?;
        self.storage
            .metadata(&key)?
            .with_context(|| format!("missing object {id}"))
    }

    pub fn refresh_object_metadata(&self) -> Result<()> {
        self.storage.refresh_objects()
    }

    pub fn visit_file_chunks(
        &self,
        file: &FileRecord,
        mut visitor: impl FnMut(&[u8]) -> Result<()>,
    ) -> Result<()> {
        validate_logical_path(&file.logical_path)?;
        let mut hasher = blake3::Hasher::new();
        let mut size = 0_u64;
        for chunk in &file.chunks {
            let raw = self.read_chunk(&chunk.id)?;
            if raw.len() as u64 != chunk.raw_size {
                bail!(
                    "raw size mismatch for object {}: got {}, expected {}",
                    chunk.id,
                    raw.len(),
                    chunk.raw_size
                );
            }
            let id = blake3::hash(&raw).to_hex().to_string();
            if id != chunk.id {
                bail!(
                    "content hash mismatch for object {}: decoded as {}",
                    chunk.id,
                    id
                );
            }
            size = size
                .checked_add(raw.len() as u64)
                .context("file size overflow")?;
            hasher.update(&raw);
            visitor(&raw)?;
        }
        if size != file.size {
            bail!(
                "file size mismatch for {}: got {}, expected {}",
                file.logical_path,
                size,
                file.size
            );
        }
        let hash = hasher.finalize().to_hex().to_string();
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

    pub fn read_file(&self, file: &FileRecord) -> Result<Vec<u8>> {
        let capacity = usize::try_from(file.size)
            .context("file is too large to materialize on this platform")?;
        let mut contents = Vec::with_capacity(capacity);
        self.visit_file_chunks(file, |chunk| {
            contents.extend_from_slice(chunk);
            Ok(())
        })?;
        Ok(contents)
    }

    pub fn record_full_verification(&self, snapshot_id: &str) -> Result<DateTime<Utc>> {
        validate_snapshot_id(snapshot_id)?;
        let verified_at = Utc::now();
        let receipt = VerificationReceipt {
            format_version: VERIFICATION_RECEIPT_VERSION,
            vault_id: self.vault_id,
            snapshot_id: snapshot_id.to_string(),
            verified_at,
            level: "full".to_string(),
        };
        let mut contents =
            serde_json::to_vec_pretty(&receipt).context("failed to encode verification receipt")?;
        contents.push(b'\n');
        write_private_atomic(&self.verification_path(snapshot_id)?, &contents)?;
        Ok(verified_at)
    }

    pub fn full_verification_time(&self, snapshot_id: &str) -> Result<Option<DateTime<Utc>>> {
        let path = self.verification_path(snapshot_id)?;
        let contents = match fs::read(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read receipt {}", path.display()));
            }
        };
        let receipt: VerificationReceipt = serde_json::from_slice(&contents)
            .with_context(|| format!("failed to parse receipt {}", path.display()))?;
        if receipt.format_version != VERIFICATION_RECEIPT_VERSION
            || receipt.vault_id != self.vault_id
            || receipt.snapshot_id != snapshot_id
            || receipt.level != "full"
        {
            bail!("invalid verification receipt {}", path.display());
        }
        Ok(Some(receipt.verified_at))
    }

    pub fn publish_manifest(&self, manifest: &Manifest) -> Result<()> {
        validate_snapshot_id(&manifest.snapshot_id)?;
        let mut plaintext =
            serde_json::to_vec_pretty(manifest).context("failed to encode snapshot manifest")?;
        plaintext.push(b'\n');
        let encoded = self.codec.encrypt(&plaintext)?;
        let key = self.manifest_key(&manifest.snapshot_id)?;
        self.storage.put(&key, &encoded, false)?;
        self.storage.put(
            "refs/latest",
            format!("{}\n", manifest.snapshot_id).as_bytes(),
            true,
        )?;
        self.storage
            .publish(&format!("Akeep snapshot {}", manifest.snapshot_id))
    }

    pub fn load_manifest(&self, reference: &str) -> Result<Manifest> {
        let snapshot_id = self.resolve_reference(reference)?;
        self.load_manifest_by_id(&snapshot_id)
    }

    pub fn load_manifest_by_id(&self, snapshot_id: &str) -> Result<Manifest> {
        let key = self.manifest_key(snapshot_id)?;
        let encoded = self.storage.get(&key)?;
        let plaintext = self.codec.decrypt(&encoded)?;
        serde_json::from_slice(&plaintext)
            .with_context(|| format!("failed to parse manifest {snapshot_id}"))
    }

    pub fn latest_snapshot_id(&self) -> Result<Option<String>> {
        if self.storage.metadata("refs/latest")?.is_none() {
            return Ok(None);
        }
        let encoded = self.storage.get("refs/latest")?;
        let snapshot_id = std::str::from_utf8(&encoded)
            .context("HEAD reference is not UTF-8")?
            .trim();
        validate_snapshot_id(snapshot_id)?;
        Ok(Some(snapshot_id.to_string()))
    }

    pub fn list_manifests(&self) -> Result<Vec<Manifest>> {
        self.storage
            .list("manifests/")?
            .into_iter()
            .filter(|key| key.ends_with(self.manifest_suffix()))
            .map(|key| {
                let encoded = self.storage.get(&key)?;
                let plaintext = self.codec.decrypt(&encoded)?;
                serde_json::from_slice(&plaintext)
                    .with_context(|| format!("failed to parse manifest {key}"))
            })
            .collect()
    }

    pub fn history_manifests(&self) -> Result<Vec<Manifest>> {
        let Some(mut snapshot_id) = self.latest_snapshot_id()? else {
            return Ok(Vec::new());
        };
        let mut manifests = Vec::new();
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(snapshot_id.clone()) {
                bail!("commit history contains a parent cycle at {snapshot_id}");
            }
            let manifest = self.load_manifest_by_id(&snapshot_id)?;
            manifest.validate(self.vault_id)?;
            let parent = manifest.parent.clone();
            manifests.push(manifest);
            let Some(parent) = parent else {
                break;
            };
            snapshot_id = parent;
        }
        Ok(manifests)
    }

    pub fn copy_repository_to(&self, destination: &Path) -> Result<RepositoryCopyStats> {
        let target = Storage::from_config(
            &crate::config::TargetConfig::Filesystem {
                path: destination.to_path_buf(),
            },
            destination,
        )?;
        if !target.list("")?.is_empty() {
            bail!(
                "clone repository directory is not empty: {}",
                destination.display()
            );
        }
        let keys = self.storage.list("")?;
        for required in ["vault.json", "refs/latest"] {
            if !keys.iter().any(|key| key == required) {
                bail!("source repository is missing {required}");
            }
        }

        let mut stored_bytes = 0_u64;
        for key in &keys {
            let contents = self.storage.get(key)?;
            let expected_hash = blake3::hash(&contents);
            target.put(key, &contents, false)?;
            let copied = target.get(key)?;
            if copied.len() != contents.len() || blake3::hash(&copied) != expected_hash {
                bail!("clone transport verification failed for repository object {key}");
            }
            stored_bytes = stored_bytes
                .checked_add(contents.len() as u64)
                .context("cloned repository byte count overflow")?;
        }

        Ok(RepositoryCopyStats {
            objects: keys.len() as u64,
            stored_bytes,
        })
    }

    fn initialize_layout(&self) -> Result<()> {
        create_private_directory(&self.state_root)?;
        for relative in ["locks", "staging", "verification"] {
            create_private_directory(&self.state_root.join(relative))?;
        }
        Ok(())
    }

    fn initialize_metadata(&self, config: &Config) -> Result<()> {
        let key = "vault.json";
        let expected = VaultMetadata {
            format_version: VAULT_METADATA_VERSION,
            vault_id: config.vault.id,
            encryption: self.codec.mode(),
            recipient: config.encryption.recipient.clone(),
        };
        if self.storage.metadata(key)?.is_some() {
            let contents = self.storage.get(key)?;
            let actual: VaultMetadata =
                serde_json::from_slice(&contents).context("failed to parse vault metadata")?;
            if actual.format_version != expected.format_version
                || actual.vault_id != expected.vault_id
                || actual.encryption != expected.encryption
                || actual
                    .recipient
                    .as_ref()
                    .is_some_and(|recipient| Some(recipient) != expected.recipient.as_ref())
            {
                bail!("vault metadata does not match the active configuration");
            }
            if actual.recipient.is_none() && expected.recipient.is_some() {
                if let Some(snapshot_id) = self.latest_snapshot_id()? {
                    self.load_manifest_by_id(&snapshot_id)?;
                }
                let mut encoded = serde_json::to_vec_pretty(&expected)
                    .context("failed to encode vault metadata")?;
                encoded.push(b'\n');
                self.storage.put(key, &encoded, true)?;
                self.storage.publish("Record Akeep age recipient")?;
            }
            return Ok(());
        }

        let mut encoded =
            serde_json::to_vec_pretty(&expected).context("failed to encode vault metadata")?;
        encoded.push(b'\n');
        self.storage.put(key, &encoded, false)?;
        self.storage.publish("Initialize Akeep repository")
    }

    fn resolve_reference(&self, reference: &str) -> Result<String> {
        if matches!(reference, "HEAD" | "latest") {
            return self
                .latest_snapshot_id()?
                .context("repository has no commits");
        }
        if let Some(distance) = reference.strip_prefix("HEAD~") {
            let distance = distance
                .parse::<usize>()
                .with_context(|| format!("invalid commit reference {reference:?}"))?;
            let mut snapshot_id = self
                .latest_snapshot_id()?
                .context("repository has no commits")?;
            for _ in 0..distance {
                let manifest = self.load_manifest_by_id(&snapshot_id)?;
                snapshot_id = manifest.parent.with_context(|| {
                    format!("commit reference {reference:?} is outside available history")
                })?;
            }
            return Ok(snapshot_id);
        }
        validate_snapshot_id(reference)?;
        Ok(reference.to_string())
    }

    fn manifest_key(&self, snapshot_id: &str) -> Result<String> {
        validate_snapshot_id(snapshot_id)?;
        Ok(format!("manifests/{snapshot_id}{}", self.manifest_suffix()))
    }

    fn object_key(&self, id: &str) -> Result<String> {
        validate_object_id(id)?;
        Ok(format!(
            "objects/{}/{}{}",
            &id[..2],
            &id[2..],
            self.object_suffix()
        ))
    }

    fn object_suffix(&self) -> &'static str {
        match self.codec.mode() {
            EncryptionMode::None => ".zst",
            EncryptionMode::Age => ".zst.age",
        }
    }

    fn manifest_suffix(&self) -> &'static str {
        match self.codec.mode() {
            EncryptionMode::None => ".json",
            EncryptionMode::Age => ".json.age",
        }
    }

    fn verification_path(&self, snapshot_id: &str) -> Result<PathBuf> {
        validate_snapshot_id(snapshot_id)?;
        Ok(self
            .state_root
            .join("verification")
            .join(format!("{snapshot_id}.json")))
    }
}

fn open_private_rw(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))
}

fn write_private_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("path {} has no parent", path.display()))?;
    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temporary file in {}", parent.display()))?;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(temporary.path(), path)
        .with_context(|| format!("failed to publish {}", path.display()))?;
    sync_directory(parent)
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)
        .with_context(|| format!("failed to open directory {}", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync directory {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use chrono::{DateTime, Utc};
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;
    use crate::config::{
        ArchiveConfig, EncryptionConfig, EncryptionMode, SourceOverrides, TargetConfig, VaultConfig,
    };
    use crate::manifest::{ArchiveDescriptor, MANIFEST_FORMAT_VERSION, Manifest, SnapshotStats};

    #[test]
    fn stores_and_reads_a_deduplicated_chunk() {
        let temp = TempDir::new().unwrap();
        let vault = Vault::open(&config(temp.path())).unwrap();

        let first = vault.store_chunk(b"repeated", 3).unwrap();
        let second = vault.store_chunk(b"repeated", 3).unwrap();
        assert!(first.is_new);
        assert!(!second.is_new);
        assert_eq!(first.id, second.id);
        assert_eq!(vault.read_chunk(&first.id).unwrap(), b"repeated");
    }

    #[test]
    fn refuses_to_reuse_a_corrupted_object() {
        let temp = TempDir::new().unwrap();
        let vault = Vault::open(&config(temp.path())).unwrap();
        let stored = vault.store_chunk(b"important", 3).unwrap();
        let key = vault.object_key(&stored.id).unwrap();
        fs::write(vault.filesystem_root().unwrap().join(key), b"corrupt").unwrap();

        assert!(vault.store_chunk(b"important", 3).is_err());
    }

    #[test]
    fn publishes_and_resolves_latest_manifest() {
        let temp = TempDir::new().unwrap();
        let config = config(temp.path());
        let vault = Vault::open(&config).unwrap();
        let manifest = Manifest {
            format_version: MANIFEST_FORMAT_VERSION,
            vault_id: config.vault.id,
            snapshot_id: "snapshot-1".to_string(),
            parent: None,
            message: None,
            created_at: Utc::now(),
            hostname: "test".to_string(),
            archive: ArchiveDescriptor {
                chunk_algorithm: "fixed".to_string(),
                chunk_size: 4,
                compression: "zstd".to_string(),
                compression_level: 3,
                encryption: "none".to_string(),
            },
            providers: Vec::new(),
            files: Vec::new(),
            stats: SnapshotStats::default(),
        };
        vault.publish_manifest(&manifest).unwrap();

        assert_eq!(vault.load_manifest("latest").unwrap(), manifest);
    }

    fn config(root: &Path) -> Config {
        Config {
            format_version: 1,
            vault: VaultConfig {
                id: Uuid::nil(),
                created_at: DateTime::from_timestamp(0, 0).unwrap(),
                state_path: PathBuf::from(root).with_extension("state"),
            },
            target: TargetConfig::Filesystem {
                path: PathBuf::from(root),
            },
            archive: ArchiveConfig {
                chunk_size: 4,
                compression_level: 3,
                workers: None,
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
