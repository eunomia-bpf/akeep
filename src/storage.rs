use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::Deserialize;
use tempfile::NamedTempFile;
use uuid::Uuid;
use walkdir::WalkDir;

use crate::config::{EncryptionMode, TargetConfig, create_private_directory};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectMetadata {
    pub size: u64,
}

pub enum Storage {
    Filesystem(FilesystemStorage),
    S3(Box<S3Storage>),
    Git(Box<GitStorage>),
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
    pending_objects: Mutex<PendingObjects>,
}

pub struct GitStorage {
    git_cli: PathBuf,
    repository: String,
    branch: String,
    checkout: PathBuf,
    files: FilesystemStorage,
    remote_head: Mutex<Option<String>>,
    _cache_lock: File,
}

#[derive(Default)]
struct PendingObjects {
    root: Option<PathBuf>,
    count: usize,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredVaultIdentity {
    format_version: u32,
    vault_id: Uuid,
    encryption: EncryptionMode,
    #[serde(default)]
    recipient: Option<String>,
}

impl Storage {
    pub fn from_config(config: &TargetConfig, state_root: &Path) -> Result<Self> {
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
            } => Ok(Self::S3(Box::new(S3Storage {
                aws_cli: aws_cli.clone().unwrap_or_else(|| PathBuf::from("aws")),
                bucket: bucket.clone(),
                prefix: prefix.clone(),
                region: region.clone(),
                profile: profile.clone(),
                endpoint_url: endpoint_url.clone(),
                object_cache: Mutex::new(None),
                pending_objects: Mutex::new(PendingObjects::default()),
            }))),
            TargetConfig::Git {
                repository,
                branch,
                git_cli,
            } => Ok(Self::Git(Box::new(GitStorage::open(
                git_cli.clone().unwrap_or_else(|| PathBuf::from("git")),
                repository.clone(),
                branch.clone(),
                state_root,
            )?))),
        }
    }

    pub fn get(&self, key: &str) -> Result<Vec<u8>> {
        validate_key(key)?;
        match self {
            Self::Filesystem(storage) => storage.get(key),
            Self::S3(storage) => storage.get(key),
            Self::Git(storage) => storage.files.get(key),
        }
    }

    pub fn metadata(&self, key: &str) -> Result<Option<ObjectMetadata>> {
        validate_key(key)?;
        match self {
            Self::Filesystem(storage) => storage.metadata(key),
            Self::S3(storage) => storage.metadata(key),
            Self::Git(storage) => storage.files.metadata(key),
        }
    }

    pub fn put(&self, key: &str, contents: &[u8], overwrite: bool) -> Result<()> {
        validate_key(key)?;
        match self {
            Self::Filesystem(storage) => storage.put(key, contents, overwrite),
            Self::S3(storage) => storage.put(key, contents, overwrite),
            Self::Git(storage) => storage.files.put(key, contents, overwrite),
        }
    }

    pub fn list(&self, prefix: &str) -> Result<Vec<String>> {
        validate_prefix(prefix)?;
        match self {
            Self::Filesystem(storage) => storage.list(prefix),
            Self::S3(storage) => storage.list(prefix),
            Self::Git(storage) => storage.files.list(prefix),
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
            Self::Git(storage) => storage.check_readable(),
        }
    }

    pub fn configure_object_staging(&self, path: &Path) -> Result<()> {
        match self {
            Self::Filesystem(_) | Self::Git(_) => Ok(()),
            Self::S3(storage) => storage.configure_object_staging(path),
        }
    }

    pub fn flush_pending_objects(&self) -> Result<()> {
        match self {
            Self::Filesystem(_) | Self::Git(_) => Ok(()),
            Self::S3(storage) => storage.flush_pending_objects(),
        }
    }

    pub fn refresh_objects(&self) -> Result<()> {
        match self {
            Self::Filesystem(_) | Self::Git(_) => Ok(()),
            Self::S3(storage) => storage.refresh_objects(),
        }
    }

    pub fn versioning_status(&self) -> Result<Option<String>> {
        match self {
            Self::Filesystem(_) | Self::Git(_) => Ok(None),
            Self::S3(storage) => storage.versioning_status().map(Some),
        }
    }

    pub fn filesystem_root(&self) -> Option<&Path> {
        match self {
            Self::Filesystem(storage) => Some(&storage.root),
            Self::S3(_) | Self::Git(_) => None,
        }
    }

    pub fn description(&self) -> String {
        match self {
            Self::Filesystem(storage) => storage.root.display().to_string(),
            Self::S3(storage) => format!("s3://{}/{}/", storage.bucket, storage.prefix),
            Self::Git(storage) => storage.description(),
        }
    }

    pub fn publish(&self, message: &str) -> Result<()> {
        match self {
            Self::Filesystem(_) | Self::S3(_) => Ok(()),
            Self::Git(storage) => storage.publish(message),
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

pub(crate) fn stored_vault_identity(
    config: &TargetConfig,
    state_root: &Path,
) -> Result<Option<(Uuid, EncryptionMode, Option<String>)>> {
    let storage = Storage::from_config(config, state_root)?;
    if storage.metadata("vault.json")?.is_none() {
        return Ok(None);
    }
    let identity: StoredVaultIdentity = serde_json::from_slice(&storage.get("vault.json")?)
        .context("failed to parse remote vault metadata")?;
    if identity.format_version != 1 {
        bail!(
            "unsupported remote vault format version {}",
            identity.format_version
        );
    }
    Ok(Some((
        identity.vault_id,
        identity.encryption,
        identity.recipient,
    )))
}

impl GitStorage {
    fn open(
        git_cli: PathBuf,
        repository: String,
        branch: String,
        state_root: &Path,
    ) -> Result<Self> {
        if repository.is_empty() || repository.chars().any(char::is_control) {
            bail!("Git repository must not be empty");
        }
        validate_git_branch(&git_cli, &branch)?;
        let locks = state_root.join("locks");
        create_private_directory(&locks)?;
        let cache_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(locks.join("git-cache.lock"))
            .context("failed to open Git cache lock")?;
        cache_lock
            .try_lock_exclusive()
            .context("another process is using the Git repository cache")?;

        let checkout = state_root.join("git");
        let fresh = !checkout.join(".git").is_dir();
        if fresh {
            if checkout.exists() && fs::read_dir(&checkout)?.next().is_some() {
                bail!(
                    "Git cache is not an empty repository: {}",
                    checkout.display()
                );
            }
            create_private_directory(&checkout)?;
            let mut command = Command::new(&git_cli);
            command
                .args(["init", "--quiet", "--initial-branch"])
                .arg(&branch)
                .arg(&checkout);
            run_git(&mut command, "initialize repository cache")?;
            let mut command = git_command(&git_cli, &checkout);
            command.args(["remote", "add", "origin"]).arg(&repository);
            run_git(&mut command, "configure repository remote")?;
        } else {
            let mut command = git_command(&git_cli, &checkout);
            command.args(["remote", "get-url", "origin"]);
            let configured = run_git(&mut command, "inspect repository remote")?;
            if String::from_utf8_lossy(&configured.stdout).trim() != repository {
                bail!("Git cache remote does not match target.repository");
            }
        }

        let files = FilesystemStorage {
            root: checkout.join("repository"),
        };
        let storage = Self {
            git_cli,
            repository,
            branch,
            checkout,
            files,
            remote_head: Mutex::new(None),
            _cache_lock: cache_lock,
        };
        storage.synchronize(fresh)?;
        storage.reject_symlinks()?;
        create_private_directory(&storage.files.root)?;
        let attributes = b"* -text -filter -diff -merge\n";
        if storage.files.metadata(".gitattributes")?.is_some() {
            if storage.files.get(".gitattributes")? != attributes {
                bail!("Git vault has incompatible repository/.gitattributes");
            }
        } else {
            storage.files.put(".gitattributes", attributes, false)?;
        }
        Ok(storage)
    }

    fn synchronize(&self, fresh: bool) -> Result<()> {
        let remote_ref = self.remote_ref();
        let mut fetch = self.command();
        fetch
            .args(["fetch", "--quiet", "--no-tags", "origin"])
            .arg(&remote_ref);
        let output = fetch
            .output()
            .context("failed to run Git to fetch repository")?;
        let head = if output.status.success() {
            let mut command = self.command();
            command.args(["rev-parse", "--verify", "FETCH_HEAD"]);
            let output = run_git(&mut command, "resolve fetched repository")?;
            Some(String::from_utf8(output.stdout)?.trim().to_string())
        } else if self.remote_head()?.is_none() {
            if !fresh && self.local_head()?.is_some() {
                bail!("configured Git branch disappeared from the remote");
            }
            None
        } else {
            return git_failure(output, "fetch repository");
        };

        if let Some(head) = &head {
            let mut command = self.command();
            command.args(["checkout", "--quiet", "--force", "-B"]);
            command.arg(&self.branch).arg(head);
            run_git(&mut command, "update repository cache")?;
            let mut command = self.command();
            command.args(["clean", "-ffd", "--", "repository"]);
            run_git(&mut command, "clean repository cache")?;
        }
        *self
            .remote_head
            .lock()
            .map_err(|_| anyhow::anyhow!("Git remote-head lock is poisoned"))? = head;
        Ok(())
    }

    fn publish(&self, message: &str) -> Result<()> {
        let expected = self
            .remote_head
            .lock()
            .map_err(|_| anyhow::anyhow!("Git remote-head lock is poisoned"))?
            .clone();
        let actual = self.remote_head()?;
        if actual != expected {
            bail!("Git remote advanced during the Akeep operation; retry against its new HEAD");
        }

        let mut command = self.command();
        command.args(["add", "--force", "--all", "--", "repository"]);
        run_git(&mut command, "stage repository changes")?;
        let mut command = self.command();
        command.args(["diff", "--cached", "--quiet", "--", "repository"]);
        let status = command
            .status()
            .context("failed to inspect staged Git changes")?;
        if status.success() {
            return Ok(());
        }
        if status.code() != Some(1) {
            bail!("Git failed to inspect staged repository changes");
        }

        let mut command = self.command();
        command
            .args([
                "-c",
                "user.name=Akeep",
                "-c",
                "user.email=akeep@localhost",
                "-c",
                "commit.gpgSign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--quiet",
                "--message",
            ])
            .arg(message);
        run_git(&mut command, "commit repository changes")?;
        let mut command = self.command();
        command
            .args(["push", "--quiet", "origin"])
            .arg(format!("HEAD:{}", self.remote_ref()));
        let output = command
            .output()
            .context("failed to run Git to push repository")?;
        if !output.status.success() {
            self.restore_after_failed_push(expected.as_deref())?;
            return git_failure(output, "push repository");
        }
        let head = self
            .local_head()?
            .context("Git commit has no HEAD after push")?;
        *self
            .remote_head
            .lock()
            .map_err(|_| anyhow::anyhow!("Git remote-head lock is poisoned"))? = Some(head);
        Ok(())
    }

    fn restore_after_failed_push(&self, previous: Option<&str>) -> Result<()> {
        let mut command = self.command();
        if let Some(previous) = previous {
            command.args(["reset", "--hard", "--quiet"]).arg(previous);
        } else {
            command.args(["update-ref", "-d"]).arg(self.remote_ref());
        }
        run_git(&mut command, "restore cache after failed push")?;
        if previous.is_none() {
            let mut command = self.command();
            command.args(["read-tree", "--empty"]);
            run_git(&mut command, "clear unpublished Git index")?;
        }
        let mut command = self.command();
        command.args(["clean", "-ffd", "--", "repository"]);
        run_git(&mut command, "clear unpublished repository files")?;
        Ok(())
    }

    fn check_readable(&self) -> Result<()> {
        self.remote_head().map(|_| ())
    }

    fn reject_symlinks(&self) -> Result<()> {
        let mut command = self.command();
        command.args(["ls-files", "--stage", "--", "repository"]);
        let output = run_git(&mut command, "inspect repository paths")?;
        if String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.starts_with("120000 ") || line.starts_with("160000 "))
        {
            bail!("symlink or submodule found in Git vault repository");
        }
        Ok(())
    }

    fn local_head(&self) -> Result<Option<String>> {
        let mut command = self.command();
        command.args(["rev-parse", "--verify", "HEAD"]);
        optional_git_revision(&mut command, "inspect local Git HEAD", 128)
    }

    fn remote_head(&self) -> Result<Option<String>> {
        let mut command = self.command();
        command
            .args(["ls-remote", "--exit-code", "--heads", "origin"])
            .arg(self.remote_ref());
        optional_git_revision(&mut command, "inspect remote Git HEAD", 2)
    }

    fn remote_ref(&self) -> String {
        format!("refs/heads/{}", self.branch)
    }

    fn command(&self) -> Command {
        git_command(&self.git_cli, &self.checkout)
    }

    fn description(&self) -> String {
        git_target_description(&self.repository, &self.branch)
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
        if !overwrite && key.starts_with("objects/") {
            let root = self
                .pending_objects
                .lock()
                .map_err(|_| anyhow::anyhow!("S3 pending-object lock is poisoned"))?
                .root
                .clone();
            if let Some(root) = root {
                let path = root.join(key);
                let parent = path
                    .parent()
                    .with_context(|| format!("pending object {} has no parent", path.display()))?;
                create_private_directory(parent)?;
                atomic_create(&path, contents)?;
                let metadata = ObjectMetadata {
                    size: contents.len() as u64,
                };
                self.pending_objects
                    .lock()
                    .map_err(|_| anyhow::anyhow!("S3 pending-object lock is poisoned"))?
                    .count += 1;
                self.object_cache
                    .lock()
                    .map_err(|_| anyhow::anyhow!("S3 object cache lock is poisoned"))?
                    .get_or_insert_with(HashMap::new)
                    .insert(key.to_string(), metadata);
                return Ok(());
            }
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

    fn configure_object_staging(&self, path: &Path) -> Result<()> {
        create_private_directory(path)?;
        let mut pending = self
            .pending_objects
            .lock()
            .map_err(|_| anyhow::anyhow!("S3 pending-object lock is poisoned"))?;
        if pending.count > 0 {
            bail!("cannot replace S3 staging while objects are pending");
        }
        pending.root = Some(path.to_path_buf());
        Ok(())
    }

    fn flush_pending_objects(&self) -> Result<()> {
        let mut pending = self
            .pending_objects
            .lock()
            .map_err(|_| anyhow::anyhow!("S3 pending-object lock is poisoned"))?;
        if pending.count == 0 {
            return Ok(());
        }
        let root = pending
            .root
            .clone()
            .context("S3 object staging is not configured")?;
        let source = root.join("objects");
        let destination = format!("s3://{}/{}/objects/", self.bucket, self.prefix);
        let mut command = self.command();
        command
            .args(["s3", "cp"])
            .arg(&source)
            .arg(&destination)
            .args(["--recursive", "--only-show-errors"]);
        run(&mut command, "upload staged S3 object batch")?;

        fs::remove_dir_all(&root)
            .with_context(|| format!("failed to clear S3 staging {}", root.display()))?;
        create_private_directory(&root)?;
        pending.count = 0;
        Ok(())
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

fn validate_git_branch(git_cli: &Path, branch: &str) -> Result<()> {
    let mut command = Command::new(git_cli);
    command.args(["check-ref-format", "--branch", branch]);
    run_git(&mut command, "validate branch name").map(|_| ())
}

fn git_command(git_cli: &Path, checkout: &Path) -> Command {
    let mut command = Command::new(git_cli);
    command
        .arg("-C")
        .arg(checkout)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LFS_SKIP_SMUDGE", "1");
    command
}

fn run_git(command: &mut Command, operation: &str) -> Result<Output> {
    let output = command
        .output()
        .with_context(|| format!("failed to run Git to {operation}"))?;
    if output.status.success() {
        Ok(output)
    } else {
        git_failure(output, operation)
    }
}

fn git_failure<T>(output: Output, operation: &str) -> Result<T> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("Git failed to {operation}: {}", stderr.trim())
}

fn optional_git_revision(
    command: &mut Command,
    operation: &str,
    missing_code: i32,
) -> Result<Option<String>> {
    let output = command
        .output()
        .with_context(|| format!("failed to run Git to {operation}"))?;
    match output.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8(output.stdout)?
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string(),
        )),
        Some(code) if code == missing_code && output.stdout.is_empty() => Ok(None),
        _ => git_failure(output, operation),
    }
}

pub(crate) fn git_target_description(repository: &str, branch: &str) -> String {
    format!("git:{}#{branch}", redact_git_remote(repository))
}

fn redact_git_remote(repository: &str) -> String {
    let Some(scheme) = repository.find("://") else {
        return repository.to_string();
    };
    let authority = scheme + 3;
    let Some(at) = repository[authority..].find('@').map(|at| authority + at) else {
        return repository.to_string();
    };
    format!("{}***{}", &repository[..authority], &repository[at..])
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
        let storage = Storage::from_config(
            &TargetConfig::Filesystem {
                path: temp.path().to_path_buf(),
            },
            temp.path(),
        )
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

    #[test]
    fn git_storage_rejects_a_concurrent_remote_advance() {
        let temp = TempDir::new().unwrap();
        let remote = temp.path().join("remote.git");
        let mut command = Command::new("git");
        command.args(["init", "--bare", "--quiet"]).arg(&remote);
        run_git(&mut command, "initialize test remote").unwrap();
        let target = TargetConfig::Git {
            repository: remote.display().to_string(),
            branch: "akeep".to_string(),
            git_cli: Some(PathBuf::from("git")),
        };
        let first = Storage::from_config(&target, &temp.path().join("first")).unwrap();
        first.put("vault.json", b"first", false).unwrap();
        first.publish("initialize").unwrap();
        let second = Storage::from_config(&target, &temp.path().join("second")).unwrap();

        first.put("refs/latest", b"first\n", true).unwrap();
        second.put("refs/latest", b"second\n", true).unwrap();
        second.publish("second writer").unwrap();
        let error = first.publish("stale writer").unwrap_err().to_string();
        assert!(error.contains("remote advanced"), "{error}");
    }
}
