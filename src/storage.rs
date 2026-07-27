use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tempfile::NamedTempFile;
use walkdir::WalkDir;

use crate::config::{TargetConfig, create_private_directory};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectMetadata {
    pub size: u64,
}

pub enum Storage {
    Filesystem(FilesystemStorage),
    S3(S3Storage),
}

pub struct FilesystemStorage {
    root: PathBuf,
}

pub struct S3Storage {
    aws_cli: PathBuf,
    bucket: String,
    prefix: String,
    region: Option<String>,
    profile: Option<String>,
    endpoint_url: Option<String>,
    object_cache: Mutex<Option<HashMap<String, ObjectMetadata>>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct S3ListResponse {
    #[serde(default)]
    contents: Vec<S3ListObject>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct S3ListObject {
    key: String,
    size: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct S3VersioningResponse {
    status: Option<String>,
}

impl Storage {
    pub fn from_config(config: &TargetConfig) -> Result<Self> {
        match config {
            TargetConfig::Filesystem { path } => {
                create_private_directory(path)?;
                Ok(Self::Filesystem(FilesystemStorage { root: path.clone() }))
            }
            TargetConfig::S3 {
                bucket,
                prefix,
                region,
                profile,
                endpoint_url,
                aws_cli,
            } => Ok(Self::S3(S3Storage {
                aws_cli: aws_cli.clone().unwrap_or_else(|| PathBuf::from("aws")),
                bucket: bucket.clone(),
                prefix: prefix.clone(),
                region: region.clone(),
                profile: profile.clone(),
                endpoint_url: endpoint_url.clone(),
                object_cache: Mutex::new(None),
            })),
        }
    }

    pub fn get(&self, key: &str) -> Result<Vec<u8>> {
        validate_key(key)?;
        match self {
            Self::Filesystem(storage) => storage.get(key),
            Self::S3(storage) => storage.get(key),
        }
    }

    pub fn metadata(&self, key: &str) -> Result<Option<ObjectMetadata>> {
        validate_key(key)?;
        match self {
            Self::Filesystem(storage) => storage.metadata(key),
            Self::S3(storage) => storage.metadata(key),
        }
    }

    pub fn put(&self, key: &str, contents: &[u8], overwrite: bool) -> Result<()> {
        validate_key(key)?;
        match self {
            Self::Filesystem(storage) => storage.put(key, contents, overwrite),
            Self::S3(storage) => storage.put(key, contents, overwrite),
        }
    }

    pub fn list(&self, prefix: &str) -> Result<Vec<String>> {
        validate_prefix(prefix)?;
        match self {
            Self::Filesystem(storage) => storage.list(prefix),
            Self::S3(storage) => storage.list(prefix),
        }
    }

    pub fn check_readable(&self) -> Result<()> {
        match self {
            Self::Filesystem(storage) => {
                fs::read_dir(&storage.root)
                    .with_context(|| format!("failed to read {}", storage.root.display()))?;
                Ok(())
            }
            Self::S3(storage) => {
                storage.list_response("", Some(1))?;
                Ok(())
            }
        }
    }

    pub fn refresh_objects(&self) -> Result<()> {
        match self {
            Self::Filesystem(_) => Ok(()),
            Self::S3(storage) => storage.refresh_objects(),
        }
    }

    pub fn versioning_status(&self) -> Result<Option<String>> {
        match self {
            Self::Filesystem(_) => Ok(None),
            Self::S3(storage) => storage.versioning_status().map(Some),
        }
    }

    pub fn filesystem_root(&self) -> Option<&Path> {
        match self {
            Self::Filesystem(storage) => Some(&storage.root),
            Self::S3(_) => None,
        }
    }

    pub fn description(&self) -> String {
        match self {
            Self::Filesystem(storage) => storage.root.display().to_string(),
            Self::S3(storage) => format!("s3://{}/{}/", storage.bucket, storage.prefix),
        }
    }
}

impl FilesystemStorage {
    fn get(&self, key: &str) -> Result<Vec<u8>> {
        let path = self.path(key);
        fs::read(&path).with_context(|| format!("failed to read object {}", path.display()))
    }

    fn metadata(&self, key: &str) -> Result<Option<ObjectMetadata>> {
        let path = self.path(key);
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => Ok(Some(ObjectMetadata {
                size: metadata.len(),
            })),
            Ok(_) => bail!("storage key is not a file: {}", path.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => {
                Err(error).with_context(|| format!("failed to inspect {}", path.display()))
            }
        }
    }

    fn put(&self, key: &str, contents: &[u8], overwrite: bool) -> Result<()> {
        let path = self.path(key);
        let parent = path
            .parent()
            .with_context(|| format!("storage path {} has no parent", path.display()))?;
        create_private_directory(parent)?;
        if overwrite {
            atomic_replace(&path, contents)
        } else {
            atomic_create(&path, contents)
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let path = self.path(prefix);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let mut keys = Vec::new();
        for entry in WalkDir::new(path).follow_links(false).sort_by_file_name() {
            let entry = entry?;
            if entry.file_type().is_symlink() {
                bail!("symlink found in vault storage: {}", entry.path().display());
            }
            if entry.file_type().is_file() {
                let relative = entry.path().strip_prefix(&self.root)?;
                keys.push(path_to_key(relative)?);
            }
        }
        keys.sort();
        Ok(keys)
    }

    fn path(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }
}

impl S3Storage {
    fn get(&self, key: &str) -> Result<Vec<u8>> {
        let uri = self.uri(key);
        let mut command = self.command();
        command.args(["s3", "cp"]).arg(uri).arg("-");
        command.arg("--only-show-errors");
        let output = run(&mut command, "download S3 object")?;
        Ok(output.stdout)
    }

    fn metadata(&self, key: &str) -> Result<Option<ObjectMetadata>> {
        if key.starts_with("objects/") {
            let mut cache = self
                .object_cache
                .lock()
                .map_err(|_| anyhow::anyhow!("S3 object cache lock is poisoned"))?;
            if cache.is_none() {
                *cache = Some(self.list_object_metadata()?);
            }
            return Ok(cache.as_ref().and_then(|cache| cache.get(key).copied()));
        }
        self.metadata_uncached(key)
    }

    fn metadata_uncached(&self, key: &str) -> Result<Option<ObjectMetadata>> {
        let full_key = self.full_key(key);
        let response = self.list_response(&full_key, Some(1))?;
        Ok(response
            .contents
            .into_iter()
            .find(|object| object.key == full_key)
            .map(|object| ObjectMetadata { size: object.size }))
    }

    fn put(&self, key: &str, contents: &[u8], overwrite: bool) -> Result<()> {
        if !overwrite && self.metadata(key)?.is_some() {
            bail!("refusing to overwrite existing S3 object {}", self.uri(key));
        }
        let mut temporary =
            NamedTempFile::new().context("failed to create temporary S3 upload file")?;
        temporary.write_all(contents)?;
        temporary.as_file().sync_all()?;

        let uri = self.uri(key);
        let mut command = self.command();
        command
            .args(["s3", "cp"])
            .arg(temporary.path())
            .arg(&uri)
            .arg("--only-show-errors");
        run(&mut command, "upload S3 object")?;
        if key.starts_with("objects/") {
            let mut cache = self
                .object_cache
                .lock()
                .map_err(|_| anyhow::anyhow!("S3 object cache lock is poisoned"))?;
            cache.get_or_insert_with(HashMap::new).insert(
                key.to_string(),
                ObjectMetadata {
                    size: contents.len() as u64,
                },
            );
        } else {
            let remote = self
                .metadata_uncached(key)?
                .with_context(|| format!("uploaded S3 object is missing: {uri}"))?;
            if remote.size != contents.len() as u64 {
                bail!(
                    "uploaded S3 object has size {}, expected {}: {}",
                    remote.size,
                    contents.len(),
                    uri
                );
            }
        }
        Ok(())
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let full_prefix = if prefix.is_empty() {
            format!("{}/", self.prefix)
        } else {
            self.full_key(prefix)
        };
        let response = self.list_response(&full_prefix, None)?;
        let base = format!("{}/", self.prefix);
        let mut keys = response
            .contents
            .into_iter()
            .filter_map(|object| object.key.strip_prefix(&base).map(str::to_string))
            .collect::<Vec<_>>();
        keys.sort();
        Ok(keys)
    }

    fn list_response(&self, prefix: &str, max_keys: Option<u32>) -> Result<S3ListResponse> {
        let mut command = self.command();
        command
            .args(["s3api", "list-objects-v2", "--bucket"])
            .arg(&self.bucket)
            .arg("--prefix")
            .arg(prefix)
            .args(["--output", "json"]);
        if let Some(max_keys) = max_keys {
            command.arg("--max-keys").arg(max_keys.to_string());
        }
        let output = run(&mut command, "list S3 objects")?;
        serde_json::from_slice(&output.stdout).context("AWS CLI returned invalid list JSON")
    }

    fn list_object_metadata(&self) -> Result<HashMap<String, ObjectMetadata>> {
        let full_prefix = self.full_key("objects/");
        let base = format!("{}/", self.prefix);
        let objects = self
            .list_response(&full_prefix, None)?
            .contents
            .into_iter()
            .filter_map(|object| {
                object
                    .key
                    .strip_prefix(&base)
                    .map(|key| (key.to_string(), ObjectMetadata { size: object.size }))
            })
            .collect::<HashMap<_, _>>();
        Ok(objects)
    }

    fn refresh_objects(&self) -> Result<()> {
        let objects = self.list_object_metadata()?;
        let mut cache = self
            .object_cache
            .lock()
            .map_err(|_| anyhow::anyhow!("S3 object cache lock is poisoned"))?;
        *cache = Some(objects);
        Ok(())
    }

    fn versioning_status(&self) -> Result<String> {
        let mut command = self.command();
        command
            .args(["s3api", "get-bucket-versioning", "--bucket"])
            .arg(&self.bucket)
            .args(["--output", "json"]);
        let output = run(&mut command, "check S3 bucket versioning")?;
        let response: S3VersioningResponse = serde_json::from_slice(&output.stdout)
            .context("AWS CLI returned invalid bucket-versioning JSON")?;
        Ok(response.status.unwrap_or_else(|| "Disabled".to_string()))
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.aws_cli);
        if let Some(profile) = &self.profile {
            command.arg("--profile").arg(profile);
        }
        if let Some(region) = &self.region {
            command.arg("--region").arg(region);
        }
        if let Some(endpoint_url) = &self.endpoint_url {
            command.arg("--endpoint-url").arg(endpoint_url);
        }
        command
    }

    fn full_key(&self, key: &str) -> String {
        format!("{}/{}", self.prefix, key)
    }

    fn uri(&self, key: &str) -> String {
        format!("s3://{}/{}", self.bucket, self.full_key(key))
    }
}

fn run(command: &mut Command, operation: &str) -> Result<Output> {
    let output = command
        .output()
        .with_context(|| format!("failed to run AWS CLI for {operation}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("AWS CLI failed to {operation}: {}", stderr.trim());
    }
    Ok(output)
}

fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() || key.starts_with('/') || key.ends_with('/') {
        bail!("invalid storage key {key:?}");
    }
    validate_components(key)
}

fn validate_prefix(prefix: &str) -> Result<()> {
    if prefix.starts_with('/') {
        bail!("invalid storage prefix {prefix:?}");
    }
    if prefix.is_empty() {
        return Ok(());
    }
    validate_components(prefix.trim_end_matches('/'))
}

fn validate_components(value: &str) -> Result<()> {
    if value
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        bail!("unsafe storage path {value:?}");
    }
    Ok(())
}

fn path_to_key(path: &Path) -> Result<String> {
    let mut key = String::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            bail!("unsafe path in filesystem storage: {}", path.display());
        };
        let component = component
            .to_str()
            .with_context(|| format!("non-UTF-8 path in filesystem storage: {}", path.display()))?;
        if !key.is_empty() {
            key.push('/');
        }
        key.push_str(component);
    }
    validate_key(&key)?;
    Ok(key)
}

fn atomic_create(path: &Path, contents: &[u8]) -> Result<()> {
    if path.exists() {
        bail!("refusing to overwrite existing file {}", path.display());
    }
    write_atomic(path, contents, false)
}

fn atomic_replace(path: &Path, contents: &[u8]) -> Result<()> {
    write_atomic(path, contents, true)
}

fn write_atomic(path: &Path, contents: &[u8], overwrite: bool) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("path {} has no parent", path.display()))?;
    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temporary file in {}", parent.display()))?;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    if overwrite {
        fs::rename(temporary.path(), path)
            .with_context(|| format!("failed to replace {}", path.display()))?;
    } else {
        temporary
            .persist_noclobber(path)
            .map_err(|error| error.error)
            .with_context(|| format!("failed to publish {}", path.display()))?;
    }
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
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn filesystem_storage_round_trips_and_lists() {
        let temp = TempDir::new().unwrap();
        let storage = Storage::from_config(&TargetConfig::Filesystem {
            path: temp.path().to_path_buf(),
        })
        .unwrap();
        storage.put("objects/aa/item", b"payload", false).unwrap();

        assert_eq!(storage.get("objects/aa/item").unwrap(), b"payload");
        assert_eq!(
            storage.metadata("objects/aa/item").unwrap(),
            Some(ObjectMetadata { size: 7 })
        );
        assert_eq!(storage.list("objects/").unwrap(), vec!["objects/aa/item"]);
        assert!(storage.put("objects/aa/item", b"new", false).is_err());
        storage.put("objects/aa/item", b"new", true).unwrap();
        assert_eq!(storage.get("objects/aa/item").unwrap(), b"new");
    }

    #[test]
    fn rejects_unsafe_keys() {
        for key in ["", "/absolute", "../escape", "safe/../escape", "safe//bad"] {
            assert!(validate_key(key).is_err(), "{key}");
        }
    }
}
