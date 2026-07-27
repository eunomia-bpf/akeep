use std::fs::{self, File};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};
use walkdir::WalkDir;

use crate::config::Config;
use crate::manifest::{ArchivedFileKind, validate_logical_path};
use crate::providers::{Provider, SourceItem, SourceKind, specifications};

#[derive(Clone, Debug)]
pub struct PreparedFile {
    pub provider: Provider,
    pub read_path: PathBuf,
    pub logical_path: String,
    pub kind: ArchivedFileKind,
    pub modified_unix_seconds: Option<i64>,
    pub modified_subsec_nanos: Option<u32>,
    pub unix_mode: Option<u32>,
}

pub fn prepare_files(config: &Config, staging: &Path) -> Result<Vec<PreparedFile>> {
    let mut files = Vec::new();
    for spec in specifications(config)? {
        for item in spec.items {
            if !item.source_path.exists() {
                continue;
            }
            prepare_item(&item, staging, &mut files)?;
        }
    }
    files.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    for pair in files.windows(2) {
        if pair[0].logical_path == pair[1].logical_path {
            bail!("duplicate logical source path {}", pair[0].logical_path);
        }
    }
    Ok(files)
}

fn prepare_item(item: &SourceItem, staging: &Path, files: &mut Vec<PreparedFile>) -> Result<()> {
    let metadata = fs::symlink_metadata(&item.source_path)
        .with_context(|| format!("failed to inspect source {}", item.source_path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "top-level source {} is a symlink; refusing to follow it",
            item.source_path.display()
        );
    }

    match item.kind {
        SourceKind::Directory => {
            if !metadata.is_dir() {
                bail!(
                    "expected directory source at {}",
                    item.source_path.display()
                );
            }
            prepare_directory(item, files)
        }
        SourceKind::File => {
            if !metadata.is_file() {
                bail!("expected file source at {}", item.source_path.display());
            }
            files.push(prepared_regular(
                item.provider,
                item.source_path.clone(),
                item.logical_path.clone(),
                &metadata,
                ArchivedFileKind::Regular,
            )?);
            Ok(())
        }
        SourceKind::Sqlite => {
            if !metadata.is_file() {
                bail!("expected SQLite source at {}", item.source_path.display());
            }
            let staged = staging.join(format!("{}.sqlite", uuid::Uuid::new_v4()));
            snapshot_sqlite(&item.source_path, &staged)?;
            files.push(prepared_regular(
                item.provider,
                staged,
                item.logical_path.clone(),
                &metadata,
                ArchivedFileKind::SqliteSnapshot,
            )?);
            Ok(())
        }
    }
}

fn prepare_directory(item: &SourceItem, files: &mut Vec<PreparedFile>) -> Result<()> {
    for entry in WalkDir::new(&item.source_path)
        .follow_links(false)
        .sort_by_file_name()
    {
        let entry = entry.with_context(|| {
            format!(
                "failed while walking source directory {}",
                item.source_path.display()
            )
        })?;
        if entry.path() == item.source_path {
            continue;
        }
        if entry.file_type().is_symlink() {
            continue;
        }
        if entry.file_type().is_dir() {
            continue;
        }
        if !entry.file_type().is_file() {
            bail!(
                "unsupported special file in source: {}",
                entry.path().display()
            );
        }
        let relative = entry.path().strip_prefix(&item.source_path)?;
        let logical_path = join_logical_path(&item.logical_path, relative)?;
        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
        files.push(prepared_regular(
            item.provider,
            entry.path().to_path_buf(),
            logical_path,
            &metadata,
            ArchivedFileKind::Regular,
        )?);
    }
    Ok(())
}

fn prepared_regular(
    provider: Provider,
    read_path: PathBuf,
    logical_path: String,
    metadata: &fs::Metadata,
    kind: ArchivedFileKind,
) -> Result<PreparedFile> {
    validate_logical_path(&logical_path)?;
    let (modified_unix_seconds, modified_subsec_nanos) = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| {
            (
                Some(duration.as_secs() as i64),
                Some(duration.subsec_nanos()),
            )
        })
        .unwrap_or((None, None));

    #[cfg(unix)]
    let unix_mode = {
        use std::os::unix::fs::PermissionsExt;
        Some(metadata.permissions().mode() & 0o7777)
    };
    #[cfg(not(unix))]
    let unix_mode = None;

    Ok(PreparedFile {
        provider,
        read_path,
        logical_path,
        kind,
        modified_unix_seconds,
        modified_subsec_nanos,
        unix_mode,
    })
}

fn join_logical_path(base: &str, relative: &Path) -> Result<String> {
    let mut result = base.to_string();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            bail!("unsafe source-relative path {}", relative.display());
        };
        let component = component
            .to_str()
            .with_context(|| format!("non-UTF-8 source path {}", relative.display()))?;
        result.push('/');
        result.push_str(component);
    }
    validate_logical_path(&result)?;
    Ok(result)
}

fn snapshot_sqlite(source: &Path, destination: &Path) -> Result<()> {
    let source_connection =
        Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("failed to open SQLite source {}", source.display()))?;
    source_connection
        .execute_batch("BEGIN DEFERRED")
        .with_context(|| format!("failed to start SQLite snapshot for {}", source.display()))?;
    let _: i64 = source_connection
        .query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| row.get(0))
        .with_context(|| format!("failed to pin SQLite snapshot for {}", source.display()))?;
    let mut destination_connection = Connection::open(destination)
        .with_context(|| format!("failed to create SQLite snapshot {}", destination.display()))?;
    destination_connection
        .pragma_update(None, "journal_mode", "OFF")
        .with_context(|| {
            format!(
                "failed to configure SQLite snapshot {}",
                destination.display()
            )
        })?;
    destination_connection
        .pragma_update(None, "synchronous", "OFF")
        .with_context(|| {
            format!(
                "failed to configure SQLite snapshot {}",
                destination.display()
            )
        })?;
    {
        let backup = Backup::new(&source_connection, &mut destination_connection)
            .with_context(|| format!("failed to start SQLite backup for {}", source.display()))?;
        let deadline = std::time::Instant::now() + Duration::from_secs(5 * 60);
        loop {
            match backup
                .step(-1)
                .with_context(|| format!("SQLite backup failed for {}", source.display()))?
            {
                StepResult::Done => break,
                StepResult::More => continue,
                StepResult::Busy | StepResult::Locked => {
                    if std::time::Instant::now() >= deadline {
                        bail!(
                            "SQLite backup remained busy for five minutes: {}",
                            source.display()
                        );
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => bail!("SQLite backup returned an unsupported step state"),
            }
        }
    }
    source_connection
        .execute_batch("ROLLBACK")
        .with_context(|| format!("failed to close SQLite snapshot for {}", source.display()))?;

    let mut statement = destination_connection
        .prepare("PRAGMA integrity_check")
        .with_context(|| format!("failed to check SQLite snapshot {}", destination.display()))?;
    let results: Vec<String> = statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if results.as_slice() != ["ok"] {
        bail!(
            "SQLite integrity check failed for {}: {}",
            source.display(),
            results.join("; ")
        );
    }
    drop(statement);
    drop(destination_connection);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o600))?;
    }
    File::open(destination)
        .with_context(|| format!("failed to open SQLite snapshot {}", destination.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync SQLite snapshot {}", destination.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use chrono::DateTime;
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;
    use crate::config::{
        ArchiveConfig, EncryptionConfig, EncryptionMode, SourceOverrides, TargetConfig, VaultConfig,
    };

    #[test]
    fn snapshots_sqlite_with_integrity() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source.sqlite");
        let destination = temp.path().join("copy.sqlite");
        let connection = Connection::open(&source).unwrap();
        connection
            .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
            .unwrap();
        connection
            .execute("INSERT INTO messages VALUES ('hello')", [])
            .unwrap();

        snapshot_sqlite(&source, &destination).unwrap();
        let copy = Connection::open(destination).unwrap();
        let body: String = copy
            .query_row("SELECT body FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(body, "hello");
    }

    #[test]
    fn snapshots_sqlite_during_concurrent_wal_writes() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("live.sqlite");
        let destination = temp.path().join("snapshot.sqlite");
        let mut connection = Connection::open(&source).unwrap();
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        connection
            .execute(
                "CREATE TABLE events (id INTEGER PRIMARY KEY, body TEXT NOT NULL)",
                [],
            )
            .unwrap();
        {
            let transaction = connection.transaction().unwrap();
            for index in 0..10_000 {
                transaction
                    .execute(
                        "INSERT INTO events (body) VALUES (?1)",
                        [format!("seed-{index}")],
                    )
                    .unwrap();
            }
            transaction.commit().unwrap();
        }
        drop(connection);

        let stop = Arc::new(AtomicBool::new(false));
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let writer_source = source.clone();
        let writer_stop = Arc::clone(&stop);
        let writer = thread::spawn(move || {
            let connection = Connection::open(writer_source).unwrap();
            connection.busy_timeout(Duration::from_secs(2)).unwrap();
            connection
                .pragma_update(None, "synchronous", "NORMAL")
                .unwrap();
            connection
                .execute("INSERT INTO events (body) VALUES ('writer-ready')", [])
                .unwrap();
            ready_sender.send(()).unwrap();
            while !writer_stop.load(Ordering::Relaxed) {
                let _ = connection.execute("INSERT INTO events (body) VALUES ('concurrent')", []);
                thread::sleep(Duration::from_millis(1));
            }
        });

        ready_receiver
            .recv_timeout(Duration::from_secs(15))
            .unwrap();
        snapshot_sqlite(&source, &destination).unwrap();
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();

        let snapshot = Connection::open(destination).unwrap();
        let integrity: String = snapshot
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        let (count, maximum): (i64, i64) = snapshot
            .query_row("SELECT COUNT(*), MAX(id) FROM events", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert!(count > 1);
        assert_eq!(count, maximum);
    }

    #[test]
    fn prepares_only_whitelisted_files() {
        let temp = TempDir::new().unwrap();
        let claude = temp.path().join("claude");
        let staging = temp.path().join("staging");
        fs::create_dir_all(claude.join("projects/demo")).unwrap();
        fs::create_dir_all(&staging).unwrap();
        fs::write(claude.join("projects/demo/session.jsonl"), b"session").unwrap();
        fs::write(claude.join(".credentials.json"), b"secret").unwrap();

        let mut config = sample_config(temp.path().join("vault"));
        config.sources.claude_home = Some(claude);
        config.sources.codex_home = Some(temp.path().join("missing-codex"));
        config.sources.grok_home = Some(temp.path().join("missing-grok"));
        config.sources.kimi_home = Some(temp.path().join("missing-kimi"));
        config.sources.opencode_share = Some(temp.path().join("missing-opencode-share"));
        config.sources.opencode_state = Some(temp.path().join("missing-opencode-state"));

        let files = prepare_files(&config, &staging).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0].logical_path,
            "claude-code/projects/demo/session.jsonl"
        );
    }

    fn sample_config(target: PathBuf) -> Config {
        Config {
            format_version: 1,
            vault: VaultConfig {
                id: Uuid::nil(),
                created_at: DateTime::from_timestamp(0, 0).unwrap(),
                state_path: target.with_extension("state"),
            },
            target: TargetConfig::Filesystem { path: target },
            archive: ArchiveConfig {
                chunk_size: 4,
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
