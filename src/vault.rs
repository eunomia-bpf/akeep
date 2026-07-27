use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use tempfile::NamedTempFile;

use crate::config::{Config, create_private_directory};
use crate::manifest::{Manifest, validate_object_id, validate_snapshot_id};

const OBJECT_EXTENSION: &str = "zst";

pub struct Vault {
    root: PathBuf,
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

impl Vault {
    pub fn open(config: &Config) -> Result<Self> {
        let vault = Self {
            root: config.target.path.clone(),
        };
        vault.initialize_layout()?;
        Ok(vault)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn acquire_backup_lock(&self) -> Result<VaultLock> {
        let lock_path = self.root.join("locks").join("backup.lock");
        let file = open_private_rw(&lock_path)?;
        file.try_lock_exclusive()
            .with_context(|| format!("another backup holds {}", lock_path.display()))?;
        Ok(VaultLock { _file: file })
    }

    pub fn staging_directory(&self) -> Result<tempfile::TempDir> {
        tempfile::Builder::new()
            .prefix("backup-")
            .tempdir_in(self.root.join("staging"))
            .context("failed to create private backup staging directory")
    }

    pub fn store_chunk(&self, raw: &[u8], compression_level: i32) -> Result<StoredChunk> {
        let id = blake3::hash(raw).to_hex().to_string();
        let path = self.object_path(&id)?;
        if path.exists() {
            let metadata = fs::metadata(&path)
                .with_context(|| format!("failed to inspect object {}", path.display()))?;
            let decoded = self.read_chunk(&id)?;
            if decoded != raw {
                bail!("existing object {id} failed content verification");
            }
            return Ok(StoredChunk {
                id,
                stored_size: metadata.len(),
                is_new: false,
            });
        }

        let parent = path
            .parent()
            .with_context(|| format!("object path {} has no parent", path.display()))?;
        create_private_directory(parent)?;
        let encoded = zstd::stream::encode_all(Cursor::new(raw), compression_level)
            .context("failed to compress archive chunk")?;
        let stored_size = encoded.len() as u64;
        atomic_create(&path, &encoded)?;
        let decoded = self.read_chunk(&id)?;
        if decoded != raw {
            bail!("newly written object {id} failed content verification");
        }

        Ok(StoredChunk {
            id,
            stored_size,
            is_new: true,
        })
    }

    pub fn read_chunk(&self, id: &str) -> Result<Vec<u8>> {
        let path = self.object_path(id)?;
        let file = File::open(&path)
            .with_context(|| format!("missing or unreadable object {}", path.display()))?;
        zstd::stream::decode_all(file).with_context(|| format!("failed to decompress object {id}"))
    }

    pub fn object_metadata(&self, id: &str) -> Result<fs::Metadata> {
        let path = self.object_path(id)?;
        fs::metadata(&path)
            .with_context(|| format!("missing or unreadable object {}", path.display()))
    }

    pub fn publish_manifest(&self, manifest: &Manifest) -> Result<()> {
        validate_snapshot_id(&manifest.snapshot_id)?;
        let mut encoded =
            serde_json::to_vec_pretty(manifest).context("failed to encode snapshot manifest")?;
        encoded.push(b'\n');
        let path = self.manifest_path(&manifest.snapshot_id)?;
        atomic_create(&path, &encoded)?;
        atomic_replace(
            &self.root.join("refs").join("latest"),
            format!("{}\n", manifest.snapshot_id).as_bytes(),
        )?;
        Ok(())
    }

    pub fn load_manifest(&self, reference: &str) -> Result<Manifest> {
        let snapshot_id = self.resolve_reference(reference)?;
        let path = self.manifest_path(&snapshot_id)?;
        let contents = fs::read(&path)
            .with_context(|| format!("failed to read manifest {}", path.display()))?;
        serde_json::from_slice(&contents)
            .with_context(|| format!("failed to parse manifest {}", path.display()))
    }

    pub fn list_manifests(&self) -> Result<Vec<Manifest>> {
        let directory = self.root.join("manifests");
        let mut paths = Vec::new();
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("failed to read {}", directory.display()))?
        {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry.path().extension().and_then(|value| value.to_str()) == Some("json")
            {
                paths.push(entry.path());
            }
        }
        paths.sort();

        paths
            .into_iter()
            .map(|path| {
                let contents = fs::read(&path)
                    .with_context(|| format!("failed to read manifest {}", path.display()))?;
                serde_json::from_slice(&contents)
                    .with_context(|| format!("failed to parse manifest {}", path.display()))
            })
            .collect()
    }

    fn initialize_layout(&self) -> Result<()> {
        create_private_directory(&self.root)?;
        for relative in ["objects", "manifests", "refs", "locks", "staging"] {
            create_private_directory(&self.root.join(relative))?;
        }
        Ok(())
    }

    fn resolve_reference(&self, reference: &str) -> Result<String> {
        if reference != "latest" {
            validate_snapshot_id(reference)?;
            return Ok(reference.to_string());
        }
        let path = self.root.join("refs").join("latest");
        let snapshot_id = fs::read_to_string(&path)
            .with_context(|| format!("no latest recovery point at {}", path.display()))?;
        let snapshot_id = snapshot_id.trim();
        validate_snapshot_id(snapshot_id)?;
        Ok(snapshot_id.to_string())
    }

    fn manifest_path(&self, snapshot_id: &str) -> Result<PathBuf> {
        validate_snapshot_id(snapshot_id)?;
        Ok(self
            .root
            .join("manifests")
            .join(format!("{snapshot_id}.json")))
    }

    fn object_path(&self, id: &str) -> Result<PathBuf> {
        validate_object_id(id)?;
        Ok(self.root.join("objects").join(&id[..2]).join(format!(
            "{}.{}",
            &id[2..],
            OBJECT_EXTENSION
        )))
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

fn atomic_create(path: &Path, contents: &[u8]) -> Result<()> {
    if path.exists() {
        bail!("refusing to overwrite existing file {}", path.display());
    }
    let parent = path
        .parent()
        .with_context(|| format!("path {} has no parent", path.display()))?;
    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temporary file in {}", parent.display()))?;
    temporary
        .write_all(contents)
        .with_context(|| format!("failed to write temporary file for {}", path.display()))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("failed to sync temporary file for {}", path.display()))?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to publish {}", path.display()))?;
    sync_directory(parent)
}

fn atomic_replace(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("path {} has no parent", path.display()))?;
    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temporary file in {}", parent.display()))?;
    temporary
        .write_all(contents)
        .with_context(|| format!("failed to write temporary file for {}", path.display()))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("failed to sync temporary file for {}", path.display()))?;
    fs::rename(temporary.path(), path)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    sync_directory(parent)
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(path)
            .with_context(|| format!("failed to open directory {}", path.display()))?
            .sync_all()
            .with_context(|| format!("failed to sync directory {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::{DateTime, Utc};
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;
    use crate::config::{
        ArchiveConfig, EncryptionConfig, EncryptionMode, FilesystemTarget, FilesystemTargetKind,
        SourceOverrides, VaultConfig,
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
        fs::write(vault.object_path(&stored.id).unwrap(), b"corrupt").unwrap();

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
            },
            target: FilesystemTarget {
                kind: FilesystemTargetKind::Filesystem,
                path: PathBuf::from(root),
            },
            archive: ArchiveConfig {
                chunk_size: 4,
                compression_level: 3,
            },
            encryption: EncryptionConfig {
                mode: EncryptionMode::None,
            },
            sources: SourceOverrides::default(),
        }
    }
}
