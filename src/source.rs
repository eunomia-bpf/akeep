use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use walkdir::WalkDir;

use crate::config::Config;
use crate::manifest::{ArchivedFileKind, validate_logical_path};
use crate::providers::{Provider, SourceItem, SourceKind, specifications};

/// Stable logical path for the per-commit AgentSight activity rollup.
pub const AGENTSIGHT_ACTIVITY_SUMMARY_PATH: &str = "agentsight/activity-summary.json";

/// How long AgentSight live-database snapshots wait on SQLITE_BUSY before skipping.
const AGENTSIGHT_SQLITE_BUSY_BUDGET: Duration = Duration::from_secs(15);
/// How long other provider SQLite snapshots wait before failing the commit.
const DEFAULT_SQLITE_BUSY_BUDGET: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Debug)]
pub struct PreparedFile {
    pub provider: Provider,
    pub read_path: PathBuf,
    pub logical_path: String,
    pub size_hint: u64,
    pub kind: ArchivedFileKind,
    pub modified_unix_seconds: Option<i64>,
    pub modified_subsec_nanos: Option<u32>,
    pub unix_mode: Option<u32>,
}

pub fn prepare_files(config: &Config, staging: &Path) -> Result<Vec<PreparedFile>> {
    let mut files = Vec::new();
    let mut agentsight_snapshots = Vec::new();
    for spec in specifications(config)? {
        for item in spec.items {
            if !item.source_path.exists() {
                continue;
            }
            prepare_item(&item, staging, &mut files, &mut agentsight_snapshots)?;
        }
    }
    if let Some(summary) = write_agentsight_activity_summary(staging, &agentsight_snapshots)? {
        files.push(summary);
    }
    files.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    for pair in files.windows(2) {
        if pair[0].logical_path == pair[1].logical_path {
            bail!("duplicate logical source path {}", pair[0].logical_path);
        }
    }
    Ok(files)
}

fn prepare_item(
    item: &SourceItem,
    staging: &Path,
    files: &mut Vec<PreparedFile>,
    agentsight_snapshots: &mut Vec<(String, PathBuf)>,
) -> Result<()> {
    let metadata = fs::symlink_metadata(&item.source_path)
        .with_context(|| format!("failed to inspect source {}", item.source_path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "top-level source {} is a symlink; refusing to follow it",
            item.source_path.display()
        );
    }

    match item.kind {
        SourceKind::Directory | SourceKind::SnapshotDirectory => {
            if !metadata.is_dir() {
                bail!(
                    "expected directory source at {}",
                    item.source_path.display()
                );
            }
            prepare_directory(
                item,
                staging,
                item.kind == SourceKind::SnapshotDirectory,
                files,
            )
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
            let busy_budget = if item.provider == Provider::AgentSight {
                AGENTSIGHT_SQLITE_BUSY_BUDGET
            } else {
                DEFAULT_SQLITE_BUSY_BUDGET
            };
            match snapshot_sqlite(&item.source_path, &staged, busy_budget) {
                Ok(()) => {
                    if item.provider == Provider::AgentSight {
                        agentsight_snapshots.push((item.logical_path.clone(), staged.clone()));
                    }
                    files.push(prepared_regular(
                        item.provider,
                        staged,
                        item.logical_path.clone(),
                        &metadata,
                        ArchivedFileKind::SqliteSnapshot,
                    )?);
                    Ok(())
                }
                Err(error) if item.provider == Provider::AgentSight => {
                    eprintln!(
                        "warning: skipping AgentSight SQLite snapshot {}: {error:#}",
                        item.source_path.display()
                    );
                    let _ = fs::remove_file(&staged);
                    Ok(())
                }
                Err(error) => Err(error),
            }
        }
    }
}

fn prepare_directory(
    item: &SourceItem,
    staging: &Path,
    snapshot_files: bool,
    files: &mut Vec<PreparedFile>,
) -> Result<()> {
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
        let read_path = if snapshot_files {
            snapshot_regular_file(entry.path(), staging)?
        } else {
            entry.path().to_path_buf()
        };
        files.push(prepared_regular(
            item.provider,
            read_path,
            logical_path,
            &metadata,
            ArchivedFileKind::Regular,
        )?);
    }
    Ok(())
}

fn snapshot_regular_file(source: &Path, staging: &Path) -> Result<PathBuf> {
    let destination = staging.join(format!("{}.file", uuid::Uuid::new_v4()));
    fs::copy(source, &destination).with_context(|| {
        format!(
            "failed to snapshot volatile source {} into {}",
            source.display(),
            destination.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o600))?;
    }
    File::open(&destination)
        .with_context(|| format!("failed to open volatile snapshot {}", destination.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync volatile snapshot {}", destination.display()))?;
    Ok(destination)
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
        size_hint: metadata.len(),
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

fn snapshot_sqlite(source: &Path, destination: &Path, busy_budget: Duration) -> Result<()> {
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
        let deadline = std::time::Instant::now() + busy_budget;
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
                            "SQLite backup remained busy for {}: {}",
                            format_duration(busy_budget),
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

fn format_duration(duration: Duration) -> String {
    if duration.as_secs() >= 60 && duration.as_secs() % 60 == 0 {
        format!("{} minutes", duration.as_secs() / 60)
    } else {
        format!("{} seconds", duration.as_secs())
    }
}

/// Target number of wall-clock buckets for the activity series (few hundred max).
const ACTIVITY_SERIES_TARGET_BUCKETS: i64 = 200;
/// Floor bucket width so a short range is still readable.
const ACTIVITY_SERIES_MIN_BUCKET_MS: i64 = 60_000;
/// Cap per-session rows so a busy week cannot unbounded-grow the document.
const SESSION_ROWS_CAP: usize = 100;
/// Fixed-length normalized CPU shape per session.
const CPU_SHAPE_POINTS: usize = 16;
/// Ranked program histogram depth before folding into `other`.
const PROGRAM_TOP_N: usize = 20;

#[derive(Debug, Serialize)]
struct ActivitySummary {
    schema_version: u32,
    provider: &'static str,
    generated_at: String,
    databases: Vec<DatabaseSummary>,
    totals: TotalsSummary,
}

#[derive(Debug, Serialize)]
struct DatabaseSummary {
    logical_path: String,
    week: Option<String>,
    /// True only when at least one core part (sessions or windows) was fully
    /// recognized with the columns needed to read it. Prefer `schema_parts`.
    schema_recognized: bool,
    /// Which tables had the columns required for their contribution.
    schema_parts: SchemaPartsRecognition,
    time_range: Option<TimeRangeSummary>,
    sessions: SessionSummary,
    windows: WindowSummary,
    samples: SampleSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    activity_series: Option<ActivitySeriesSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_rows: Option<SessionRowsSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    programs: Option<ProgramsSummary>,
}

#[derive(Debug, Default, Serialize)]
struct SchemaPartsRecognition {
    tracked_sessions: bool,
    monitor_windows: bool,
    process_samples: bool,
    file_samples: bool,
    network_samples: bool,
}

#[derive(Debug, Default, Serialize)]
struct TimeRangeSummary {
    first_seen_ms: Option<i64>,
    last_seen_ms: Option<i64>,
}

#[derive(Debug, Default, Serialize)]
struct SessionSummary {
    total: u64,
    by_agent_type: BTreeMap<String, u64>,
    by_status: BTreeMap<String, u64>,
}

#[derive(Debug, Default, Serialize)]
struct WindowSummary {
    total: u64,
    total_cpu_ms: u64,
    peak_rss_bytes: u64,
    total_read_bytes: u64,
    total_write_bytes: u64,
    total_file_target_events: u64,
    total_network_target_events: u64,
}

#[derive(Debug, Default, Serialize)]
struct SampleSummary {
    process_rows: u64,
    file_rows: u64,
    network_rows: u64,
}

#[derive(Debug, Serialize)]
struct ActivitySeriesSummary {
    bucket_width_ms: i64,
    bucket_count: u64,
    time_range: SeriesTimeRange,
    buckets: Vec<ActivityBucket>,
}

#[derive(Debug, Serialize)]
struct SeriesTimeRange {
    start_ms: i64,
    end_ms: i64,
}

#[derive(Debug, Serialize)]
struct ActivityBucket {
    start_ms: i64,
    sessions: u64,
    by_agent_type: BTreeMap<String, u64>,
    cpu_ms: u64,
    read_bytes: u64,
    write_bytes: u64,
    file_targets: u64,
    network_targets: u64,
}

#[derive(Debug, Serialize)]
struct SessionRowsSummary {
    /// Total sessions present in the database (before the cap).
    total: u64,
    /// Number of rows actually included after capping.
    included: u64,
    /// How the capped subset was chosen.
    selection: &'static str,
    /// Maximum shape length used when downsampling. Each row's `cpu_shape`
    /// length is the actual point count (≤ this value); do not assume fixed.
    cpu_shape_points: u64,
    rows: Vec<SessionRow>,
}

#[derive(Debug, Serialize)]
struct SessionRow {
    /// Opaque handle for UI correlation; not derived from a path.
    id: String,
    agent_type: String,
    status: String,
    first_seen_ms: i64,
    last_seen_ms: i64,
    duration_ms: i64,
    total_cpu_ms: u64,
    peak_rss_bytes: u64,
    total_read_bytes: u64,
    total_write_bytes: u64,
    total_file_targets: u64,
    total_network_targets: u64,
    max_process_count: u64,
    /// Gap-free CPU profile normalized to [0.0, 1.0] by the session peak.
    /// Length is the actual point count: one per window when fewer than
    /// `cpu_shape_points`, else downsampled to that target. Array length is
    /// self-describing; genuine zero-CPU windows appear as 0.0.
    cpu_shape: Vec<f64>,
}

#[derive(Debug, Serialize)]
struct ProgramsSummary {
    top_n: u64,
    entries: Vec<ProgramEntry>,
}

#[derive(Debug, Serialize)]
struct ProgramEntry {
    comm: String,
    cpu_share: f64,
    sample_count: u64,
}

#[derive(Debug, Default, Serialize)]
struct TotalsSummary {
    databases: u64,
    sessions: u64,
    windows: u64,
    by_agent_type: BTreeMap<String, u64>,
    time_range: Option<TimeRangeSummary>,
    total_cpu_ms: u64,
    peak_rss_bytes: u64,
    total_read_bytes: u64,
    total_write_bytes: u64,
}

fn write_agentsight_activity_summary(
    staging: &Path,
    snapshots: &[(String, PathBuf)],
) -> Result<Option<PreparedFile>> {
    if snapshots.is_empty() {
        return Ok(None);
    }

    let mut databases = Vec::with_capacity(snapshots.len());
    let mut totals = TotalsSummary {
        databases: snapshots.len() as u64,
        ..TotalsSummary::default()
    };

    for (logical_path, path) in snapshots {
        let database = summarize_agentsight_database(logical_path, path);
        totals.sessions = totals.sessions.saturating_add(database.sessions.total);
        totals.windows = totals.windows.saturating_add(database.windows.total);
        totals.total_cpu_ms = totals
            .total_cpu_ms
            .saturating_add(database.windows.total_cpu_ms);
        totals.peak_rss_bytes = totals.peak_rss_bytes.max(database.windows.peak_rss_bytes);
        totals.total_read_bytes = totals
            .total_read_bytes
            .saturating_add(database.windows.total_read_bytes);
        totals.total_write_bytes = totals
            .total_write_bytes
            .saturating_add(database.windows.total_write_bytes);
        for (agent, count) in &database.sessions.by_agent_type {
            *totals.by_agent_type.entry(agent.clone()).or_default() += count;
        }
        if let Some(range) = &database.time_range {
            let totals_range = totals
                .time_range
                .get_or_insert_with(TimeRangeSummary::default);
            match (totals_range.first_seen_ms, range.first_seen_ms) {
                (None, Some(value)) => totals_range.first_seen_ms = Some(value),
                (Some(existing), Some(value)) => {
                    totals_range.first_seen_ms = Some(existing.min(value));
                }
                _ => {}
            }
            match (totals_range.last_seen_ms, range.last_seen_ms) {
                (None, Some(value)) => totals_range.last_seen_ms = Some(value),
                (Some(existing), Some(value)) => {
                    totals_range.last_seen_ms = Some(existing.max(value));
                }
                _ => {}
            }
        }
        databases.push(database);
    }

    let summary = ActivitySummary {
        schema_version: 2,
        provider: Provider::AgentSight.id(),
        generated_at: Utc::now().to_rfc3339(),
        databases,
        totals,
    };
    let body = serde_json::to_vec_pretty(&summary)
        .context("failed to serialize AgentSight activity summary")?;
    let destination = staging.join(format!("{}.json", uuid::Uuid::new_v4()));
    {
        let mut file = File::create(&destination).with_context(|| {
            format!(
                "failed to create AgentSight activity summary {}",
                destination.display()
            )
        })?;
        file.write_all(&body).with_context(|| {
            format!(
                "failed to write AgentSight activity summary {}",
                destination.display()
            )
        })?;
        file.sync_all().with_context(|| {
            format!(
                "failed to sync AgentSight activity summary {}",
                destination.display()
            )
        })?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o600))?;
    }

    let metadata = fs::metadata(&destination).with_context(|| {
        format!(
            "failed to inspect AgentSight activity summary {}",
            destination.display()
        )
    })?;
    Ok(Some(prepared_regular(
        Provider::AgentSight,
        destination,
        AGENTSIGHT_ACTIVITY_SUMMARY_PATH.to_string(),
        &metadata,
        ArchivedFileKind::Regular,
    )?))
}

fn summarize_agentsight_database(logical_path: &str, path: &Path) -> DatabaseSummary {
    let week = Path::new(logical_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.strip_prefix("monitor-"))
        .map(str::to_string);

    let empty = || DatabaseSummary {
        logical_path: logical_path.to_string(),
        week: week.clone(),
        schema_recognized: false,
        schema_parts: SchemaPartsRecognition::default(),
        time_range: None,
        sessions: SessionSummary::default(),
        windows: WindowSummary::default(),
        samples: SampleSummary::default(),
        activity_series: None,
        session_rows: None,
        programs: None,
    };

    let Ok(connection) = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return empty();
    };

    let sessions_recognized = table_exists(&connection, "tracked_sessions")
        && columns_include(
            &connection,
            "tracked_sessions",
            &[
                "session_id",
                "agent_type",
                "root_pid",
                "root_starttime_ticks",
                "status",
                "first_seen_ms",
                "last_seen_ms",
            ],
        );
    let windows_recognized = table_exists(&connection, "monitor_windows")
        && columns_include(
            &connection,
            "monitor_windows",
            &[
                "session_id",
                "root_pid",
                "root_starttime_ticks",
                "window_start_ms",
                "window_end_ms",
                "process_count",
                "cpu_ms",
                "rss_bytes",
                "read_bytes",
                "write_bytes",
                "file_targets",
                "network_targets",
            ],
        );
    let process_samples_recognized = table_exists(&connection, "process_samples")
        && columns_include(&connection, "process_samples", &["comm", "cpu_ms"]);
    let file_samples_recognized = table_exists(&connection, "file_samples");
    let network_samples_recognized = table_exists(&connection, "network_samples");

    let schema_parts = SchemaPartsRecognition {
        tracked_sessions: sessions_recognized,
        monitor_windows: windows_recognized,
        process_samples: process_samples_recognized,
        file_samples: file_samples_recognized,
        network_samples: network_samples_recognized,
    };
    // Honest recognition: true only when a core part is fully readable.
    let schema_recognized = sessions_recognized || windows_recognized;

    let mut sessions = SessionSummary::default();
    let mut time_range = None;
    if sessions_recognized {
        if let Ok(total) =
            connection.query_row("SELECT COUNT(*) FROM tracked_sessions", [], |row| {
                row.get::<_, i64>(0)
            })
        {
            sessions.total = total.max(0) as u64;
        }
        if let Ok(mut statement) = connection.prepare(
            "SELECT agent_type, COUNT(*) FROM tracked_sessions GROUP BY agent_type ORDER BY agent_type",
        ) {
            if let Ok(rows) = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            }) {
                for row in rows.flatten() {
                    if is_safe_label(&row.0) {
                        sessions.by_agent_type.insert(row.0, row.1.max(0) as u64);
                    }
                }
            }
        }
        if let Ok(mut statement) = connection.prepare(
            "SELECT status, COUNT(*) FROM tracked_sessions GROUP BY status ORDER BY status",
        ) {
            if let Ok(rows) = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            }) {
                for row in rows.flatten() {
                    if is_safe_label(&row.0) {
                        sessions.by_status.insert(row.0, row.1.max(0) as u64);
                    }
                }
            }
        }
        if let Ok((first, last)) = connection.query_row(
            "SELECT MIN(first_seen_ms), MAX(last_seen_ms) FROM tracked_sessions",
            [],
            |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?)),
        ) {
            if first.is_some() || last.is_some() {
                time_range = Some(TimeRangeSummary {
                    first_seen_ms: first,
                    last_seen_ms: last,
                });
            }
        }
    }

    let mut windows = WindowSummary::default();
    if windows_recognized {
        if let Ok((
            total,
            total_cpu_ms,
            peak_rss_bytes,
            total_read_bytes,
            total_write_bytes,
            total_file_target_events,
            total_network_target_events,
        )) = connection.query_row(
            "SELECT COUNT(*), COALESCE(SUM(cpu_ms), 0), COALESCE(MAX(rss_bytes), 0), \
             COALESCE(SUM(read_bytes), 0), COALESCE(SUM(write_bytes), 0), \
             COALESCE(SUM(file_targets), 0), COALESCE(SUM(network_targets), 0) \
             FROM monitor_windows",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        ) {
            windows.total = total.max(0) as u64;
            windows.total_cpu_ms = total_cpu_ms.max(0) as u64;
            windows.peak_rss_bytes = peak_rss_bytes.max(0) as u64;
            windows.total_read_bytes = total_read_bytes.max(0) as u64;
            windows.total_write_bytes = total_write_bytes.max(0) as u64;
            windows.total_file_target_events = total_file_target_events.max(0) as u64;
            windows.total_network_target_events = total_network_target_events.max(0) as u64;
        }
    }

    // Sample row counts only when the table exists; payloads are never read.
    let samples = SampleSummary {
        process_rows: count_table_rows(&connection, "process_samples"),
        file_rows: count_table_rows(&connection, "file_samples"),
        network_rows: count_table_rows(&connection, "network_samples"),
    };

    let activity_series = if windows_recognized {
        build_activity_series(&connection, sessions_recognized)
    } else {
        None
    };
    let session_rows = if sessions_recognized && windows_recognized {
        build_session_rows(&connection)
    } else if sessions_recognized {
        build_session_rows_without_windows(&connection)
    } else {
        None
    };
    let programs = if process_samples_recognized {
        build_program_histogram(&connection)
    } else {
        None
    };

    DatabaseSummary {
        logical_path: logical_path.to_string(),
        week,
        schema_recognized,
        schema_parts,
        time_range,
        sessions,
        windows,
        samples,
        activity_series,
        session_rows,
        programs,
    }
}

/// Choose a fixed wall-clock bucket width so the series stays near
/// `ACTIVITY_SERIES_TARGET_BUCKETS` buckets rather than exploding over long ranges.
fn choose_bucket_width_ms(range_ms: i64) -> i64 {
    if range_ms <= 0 {
        return ACTIVITY_SERIES_MIN_BUCKET_MS;
    }
    let raw = (range_ms + ACTIVITY_SERIES_TARGET_BUCKETS - 1) / ACTIVITY_SERIES_TARGET_BUCKETS;
    let raw = raw.max(ACTIVITY_SERIES_MIN_BUCKET_MS);
    const NICE_MS: &[i64] = &[
        60_000,           // 1 min
        5 * 60_000,       // 5 min
        15 * 60_000,      // 15 min
        30 * 60_000,      // 30 min
        60 * 60_000,      // 1 h
        2 * 60 * 60_000,  // 2 h
        3 * 60 * 60_000,  // 3 h
        6 * 60 * 60_000,  // 6 h
        12 * 60 * 60_000, // 12 h
        24 * 60 * 60_000, // 1 d
    ];
    for &width in NICE_MS {
        if width >= raw {
            return width;
        }
    }
    // Multi-week+ ranges: round up to whole days.
    let day = 24 * 60 * 60_000;
    ((raw + day - 1) / day) * day
}

#[derive(Clone)]
struct WindowRecord {
    session_id: String,
    root_pid: i64,
    root_starttime_ticks: i64,
    window_start_ms: i64,
    window_end_ms: i64,
    process_count: i64,
    cpu_ms: i64,
    rss_bytes: i64,
    read_bytes: i64,
    write_bytes: i64,
    file_targets: i64,
    network_targets: i64,
    agent_type: String,
}

fn load_windows_with_agents(
    connection: &Connection,
    sessions_recognized: bool,
) -> Option<Vec<WindowRecord>> {
    let sql = if sessions_recognized {
        "SELECT w.session_id, w.root_pid, w.root_starttime_ticks, \
                w.window_start_ms, w.window_end_ms, w.process_count, w.cpu_ms, \
                w.rss_bytes, w.read_bytes, w.write_bytes, w.file_targets, w.network_targets, \
                COALESCE(s.agent_type, '') \
         FROM monitor_windows w \
         LEFT JOIN tracked_sessions s \
           ON s.session_id = w.session_id \
          AND s.root_pid = w.root_pid \
          AND s.root_starttime_ticks = w.root_starttime_ticks"
    } else {
        "SELECT session_id, root_pid, root_starttime_ticks, \
                window_start_ms, window_end_ms, process_count, cpu_ms, \
                rss_bytes, read_bytes, write_bytes, file_targets, network_targets, \
                '' \
         FROM monitor_windows"
    };
    let mut statement = connection.prepare(sql).ok()?;
    let rows = statement
        .query_map([], |row| {
            Ok(WindowRecord {
                session_id: row.get(0)?,
                root_pid: row.get(1)?,
                root_starttime_ticks: row.get(2)?,
                window_start_ms: row.get(3)?,
                window_end_ms: row.get(4)?,
                process_count: row.get(5)?,
                cpu_ms: row.get(6)?,
                rss_bytes: row.get(7)?,
                read_bytes: row.get(8)?,
                write_bytes: row.get(9)?,
                file_targets: row.get(10)?,
                network_targets: row.get(11)?,
                agent_type: row.get(12)?,
            })
        })
        .ok()?;
    Some(rows.flatten().collect())
}

fn build_activity_series(
    connection: &Connection,
    sessions_recognized: bool,
) -> Option<ActivitySeriesSummary> {
    let windows = load_windows_with_agents(connection, sessions_recognized)?;
    if windows.is_empty() {
        return None;
    }

    let mut min_start = i64::MAX;
    let mut max_end = i64::MIN;
    for window in &windows {
        min_start = min_start.min(window.window_start_ms);
        max_end = max_end.max(window.window_end_ms);
    }
    if min_start == i64::MAX || max_end == i64::MIN {
        return None;
    }

    // Align series start down to the bucket grid for stable axis labels.
    // Target ~200 buckets; widen if nice widths still exceed 400.
    let range_ms = (max_end - min_start).max(0);
    let mut bucket_width_ms = choose_bucket_width_ms(range_ms);
    let mut series_start = min_start - min_start.rem_euclid(bucket_width_ms);
    let mut bucket_count = {
        let span = (max_end - series_start).max(1);
        ((span + bucket_width_ms - 1) / bucket_width_ms).max(1) as usize
    };
    if bucket_count > 400 {
        bucket_width_ms = ((range_ms + 199) / 200).max(ACTIVITY_SERIES_MIN_BUCKET_MS);
        // Snap up to a whole minute for axis readability.
        let minute = 60_000i64;
        bucket_width_ms = ((bucket_width_ms + minute - 1) / minute) * minute;
        series_start = min_start - min_start.rem_euclid(bucket_width_ms);
        let span = (max_end - series_start).max(1);
        bucket_count = ((span + bucket_width_ms - 1) / bucket_width_ms).max(1) as usize;
    }
    let series_end = series_start + (bucket_count as i64) * bucket_width_ms;

    #[derive(Default)]
    struct Acc {
        cpu_ms: u64,
        read_bytes: u64,
        write_bytes: u64,
        file_targets: u64,
        network_targets: u64,
        /// Distinct sessions seen in this bucket, keyed for de-dup.
        sessions: BTreeMap<(String, i64, i64), String>,
    }

    let mut buckets: Vec<Acc> = (0..bucket_count).map(|_| Acc::default()).collect();

    for window in &windows {
        // Assign metrics to the bucket of window_start to avoid double-counting.
        let metric_idx = ((window.window_start_ms - series_start) / bucket_width_ms) as isize;
        if metric_idx >= 0 && (metric_idx as usize) < bucket_count {
            let acc = &mut buckets[metric_idx as usize];
            acc.cpu_ms = acc.cpu_ms.saturating_add(window.cpu_ms.max(0) as u64);
            acc.read_bytes = acc
                .read_bytes
                .saturating_add(window.read_bytes.max(0) as u64);
            acc.write_bytes = acc
                .write_bytes
                .saturating_add(window.write_bytes.max(0) as u64);
            acc.file_targets = acc
                .file_targets
                .saturating_add(window.file_targets.max(0) as u64);
            acc.network_targets = acc
                .network_targets
                .saturating_add(window.network_targets.max(0) as u64);
        }

        // Concurrent sessions: count in every bucket the window overlaps.
        let win_start = window.window_start_ms;
        let win_end = window.window_end_ms.max(win_start);
        let first = ((win_start - series_start) / bucket_width_ms).max(0) as usize;
        let last =
            (((win_end - 1).max(win_start) - series_start) / bucket_width_ms).max(0) as usize;
        let last = last.min(bucket_count.saturating_sub(1));
        let first = first.min(last);
        let agent = if is_safe_label(&window.agent_type) {
            window.agent_type.clone()
        } else {
            String::new()
        };
        let key = (
            window.session_id.clone(),
            window.root_pid,
            window.root_starttime_ticks,
        );
        for acc in buckets.iter_mut().take(last + 1).skip(first) {
            acc.sessions.insert(key.clone(), agent.clone());
        }
    }

    let out: Vec<ActivityBucket> = buckets
        .into_iter()
        .enumerate()
        .map(|(i, acc)| {
            let mut by_agent_type = BTreeMap::new();
            for agent in acc.sessions.values() {
                if agent.is_empty() {
                    continue;
                }
                *by_agent_type.entry(agent.clone()).or_default() += 1;
            }
            ActivityBucket {
                start_ms: series_start + (i as i64) * bucket_width_ms,
                sessions: acc.sessions.len() as u64,
                by_agent_type,
                cpu_ms: acc.cpu_ms,
                read_bytes: acc.read_bytes,
                write_bytes: acc.write_bytes,
                file_targets: acc.file_targets,
                network_targets: acc.network_targets,
            }
        })
        .collect();

    Some(ActivitySeriesSummary {
        bucket_width_ms,
        bucket_count: out.len() as u64,
        time_range: SeriesTimeRange {
            start_ms: series_start,
            end_ms: series_end,
        },
        buckets: out,
    })
}

#[derive(Clone)]
struct SessionMeta {
    session_id: String,
    root_pid: i64,
    root_starttime_ticks: i64,
    agent_type: String,
    status: String,
    first_seen_ms: i64,
    last_seen_ms: i64,
}

fn load_sessions(connection: &Connection) -> Option<Vec<SessionMeta>> {
    let mut statement = connection
        .prepare(
            "SELECT session_id, root_pid, root_starttime_ticks, agent_type, status, \
                    first_seen_ms, last_seen_ms \
             FROM tracked_sessions",
        )
        .ok()?;
    let rows = statement
        .query_map([], |row| {
            Ok(SessionMeta {
                session_id: row.get(0)?,
                root_pid: row.get(1)?,
                root_starttime_ticks: row.get(2)?,
                agent_type: row.get(3)?,
                status: row.get(4)?,
                first_seen_ms: row.get(5)?,
                last_seen_ms: row.get(6)?,
            })
        })
        .ok()?;
    Some(rows.flatten().collect())
}

fn build_session_rows(connection: &Connection) -> Option<SessionRowsSummary> {
    let sessions = load_sessions(connection)?;
    let windows = load_windows_with_agents(connection, true).unwrap_or_default();
    Some(assemble_session_rows(sessions, windows))
}

fn build_session_rows_without_windows(connection: &Connection) -> Option<SessionRowsSummary> {
    let sessions = load_sessions(connection)?;
    Some(assemble_session_rows(sessions, Vec::new()))
}

fn assemble_session_rows(
    sessions: Vec<SessionMeta>,
    windows: Vec<WindowRecord>,
) -> SessionRowsSummary {
    let total = sessions.len() as u64;

    // Index windows by session identity for aggregate + shape computation.
    let mut by_session: BTreeMap<(String, i64, i64), Vec<&WindowRecord>> = BTreeMap::new();
    for window in &windows {
        by_session
            .entry((
                window.session_id.clone(),
                window.root_pid,
                window.root_starttime_ticks,
            ))
            .or_default()
            .push(window);
    }

    let mut rows: Vec<SessionRow> = sessions
        .into_iter()
        .filter(|s| is_safe_label(&s.agent_type) && is_safe_label(&s.status))
        .map(|s| {
            let key = (s.session_id.clone(), s.root_pid, s.root_starttime_ticks);
            let sess_windows = by_session.get(&key).map(Vec::as_slice).unwrap_or(&[]);
            let mut total_cpu_ms = 0u64;
            let mut peak_rss_bytes = 0u64;
            let mut total_read_bytes = 0u64;
            let mut total_write_bytes = 0u64;
            let mut total_file_targets = 0u64;
            let mut total_network_targets = 0u64;
            let mut max_process_count = 0u64;
            for w in sess_windows {
                total_cpu_ms = total_cpu_ms.saturating_add(w.cpu_ms.max(0) as u64);
                peak_rss_bytes = peak_rss_bytes.max(w.rss_bytes.max(0) as u64);
                total_read_bytes = total_read_bytes.saturating_add(w.read_bytes.max(0) as u64);
                total_write_bytes = total_write_bytes.saturating_add(w.write_bytes.max(0) as u64);
                total_file_targets =
                    total_file_targets.saturating_add(w.file_targets.max(0) as u64);
                total_network_targets =
                    total_network_targets.saturating_add(w.network_targets.max(0) as u64);
                max_process_count = max_process_count.max(w.process_count.max(0) as u64);
            }
            let duration_ms = (s.last_seen_ms - s.first_seen_ms).max(0);
            let cpu_shape = build_cpu_shape(sess_windows, CPU_SHAPE_POINTS);
            // Opaque short id: blake3 prefix of identity, never path-derived.
            let id = opaque_session_id(&s.session_id, s.root_pid, s.root_starttime_ticks);
            SessionRow {
                id,
                agent_type: s.agent_type,
                status: s.status,
                first_seen_ms: s.first_seen_ms,
                last_seen_ms: s.last_seen_ms,
                duration_ms,
                total_cpu_ms,
                peak_rss_bytes,
                total_read_bytes,
                total_write_bytes,
                total_file_targets,
                total_network_targets,
                max_process_count,
                cpu_shape,
            }
        })
        .collect();

    // Cap to the longest sessions by duration so stuck/retry loops surface;
    // ties broken by most-recent last_seen_ms.
    rows.sort_by(|a, b| {
        b.duration_ms
            .cmp(&a.duration_ms)
            .then_with(|| b.last_seen_ms.cmp(&a.last_seen_ms))
    });
    if rows.len() > SESSION_ROWS_CAP {
        rows.truncate(SESSION_ROWS_CAP);
    }
    // Stable presentation: most recent first within the capped set.
    rows.sort_by(|a, b| b.last_seen_ms.cmp(&a.last_seen_ms));

    let included = rows.len() as u64;
    SessionRowsSummary {
        total,
        included,
        selection: "longest",
        cpu_shape_points: CPU_SHAPE_POINTS as u64,
        rows,
    }
}

fn opaque_session_id(session_id: &str, root_pid: i64, root_starttime_ticks: i64) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(session_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(&root_pid.to_le_bytes());
    hasher.update(&root_starttime_ticks.to_le_bytes());
    let hash = hasher.finalize();
    // 8 hex chars: enough for UI de-dup within a weekly DB, fully opaque.
    hash.to_hex()[..8].to_string()
}

/// Build a gap-free CPU profile for sparkline rendering.
///
/// - Zero windows → empty shape.
/// - Fewer windows than `max_points` → one point per window (no zero padding).
/// - At least `max_points` windows → equal-count downsampling into `max_points`
///   bins (every bin receives ≥1 window; no artificial empty slots).
/// - Genuine zero-CPU windows remain 0.0 after peak normalization.
fn build_cpu_shape(windows: &[&WindowRecord], max_points: usize) -> Vec<f64> {
    if max_points == 0 || windows.is_empty() {
        return Vec::new();
    }

    let mut ordered: Vec<&WindowRecord> = windows.to_vec();
    ordered.sort_by_key(|w| (w.window_start_ms, w.window_end_ms));
    let samples: Vec<f64> = ordered.iter().map(|w| w.cpu_ms.max(0) as f64).collect();

    let shape = if samples.len() <= max_points {
        // Natural resolution: never upsample with artificial zeros.
        samples
    } else {
        downsample_cpu_samples(&samples, max_points)
    };
    normalize_cpu_shape(shape)
}

/// Partition `samples` into `points` contiguous groups and sum each group.
/// Requires `samples.len() >= points > 0`. Every output bin gets ≥1 sample.
fn downsample_cpu_samples(samples: &[f64], points: usize) -> Vec<f64> {
    let n = samples.len();
    debug_assert!(n >= points && points > 0);
    let mut out = Vec::with_capacity(points);
    for j in 0..points {
        let start = j * n / points;
        let end = (j + 1) * n / points;
        let sum: f64 = samples[start..end].iter().sum();
        out.push(sum);
    }
    out
}

fn normalize_cpu_shape(mut shape: Vec<f64>) -> Vec<f64> {
    let peak = shape.iter().cloned().fold(0.0f64, f64::max);
    if peak > 0.0 {
        for value in &mut shape {
            *value /= peak;
            // Round to 3 decimals to keep the document compact and stable.
            *value = (*value * 1000.0).round() / 1000.0;
        }
    }
    shape
}

/// Test/helper: shape from raw cpu_ms sequence (already time-ordered).
#[cfg(test)]
fn build_cpu_shape_from_cpu_ms(cpu_ms: &[i64], max_points: usize) -> Vec<f64> {
    if max_points == 0 || cpu_ms.is_empty() {
        return Vec::new();
    }
    let samples: Vec<f64> = cpu_ms.iter().map(|c| (*c).max(0) as f64).collect();
    let shape = if samples.len() <= max_points {
        samples
    } else {
        downsample_cpu_samples(&samples, max_points)
    };
    normalize_cpu_shape(shape)
}

fn build_program_histogram(connection: &Connection) -> Option<ProgramsSummary> {
    let mut statement = connection
        .prepare(
            "SELECT comm, COALESCE(SUM(cpu_ms), 0), COUNT(*) \
             FROM process_samples \
             GROUP BY comm",
        )
        .ok()?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .ok()?;

    let mut entries: Vec<(String, u64, u64)> = Vec::new();
    let mut total_cpu: u64 = 0;
    for row in rows.flatten() {
        let (comm, cpu, count) = row;
        if !is_safe_comm(&comm) {
            continue;
        }
        let cpu = cpu.max(0) as u64;
        let count = count.max(0) as u64;
        total_cpu = total_cpu.saturating_add(cpu);
        entries.push((comm, cpu, count));
    }
    if entries.is_empty() {
        return Some(ProgramsSummary {
            top_n: PROGRAM_TOP_N as u64,
            entries: Vec::new(),
        });
    }

    // Rank by sampled CPU, then sample count.
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.2.cmp(&a.2)));

    let mut out = Vec::new();
    let mut other_cpu = 0u64;
    let mut other_count = 0u64;
    for (i, (comm, cpu, count)) in entries.into_iter().enumerate() {
        if i < PROGRAM_TOP_N {
            let share = if total_cpu > 0 {
                (cpu as f64 / total_cpu as f64 * 1000.0).round() / 1000.0
            } else {
                0.0
            };
            out.push(ProgramEntry {
                comm,
                cpu_share: share,
                sample_count: count,
            });
        } else {
            other_cpu = other_cpu.saturating_add(cpu);
            other_count = other_count.saturating_add(count);
        }
    }
    if other_count > 0 {
        let share = if total_cpu > 0 {
            (other_cpu as f64 / total_cpu as f64 * 1000.0).round() / 1000.0
        } else {
            0.0
        };
        out.push(ProgramEntry {
            comm: "other".to_string(),
            cpu_share: share,
            sample_count: other_count,
        });
    }

    Some(ProgramsSummary {
        top_n: PROGRAM_TOP_N as u64,
        entries: out,
    })
}

fn table_exists(connection: &Connection, table: &str) -> bool {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
            [table],
            |_| Ok(()),
        )
        .is_ok()
}

fn columns_include(connection: &Connection, table: &str, required: &[&str]) -> bool {
    let Ok(mut statement) = connection.prepare(&format!("PRAGMA table_info({table})")) else {
        return false;
    };
    let Ok(rows) = statement.query_map([], |row| row.get::<_, String>(1)) else {
        return false;
    };
    let mut present = std::collections::HashSet::new();
    for name in rows.flatten() {
        present.insert(name);
    }
    required.iter().all(|column| present.contains(*column))
}

fn count_table_rows(connection: &Connection, table: &str) -> u64 {
    if !table_exists(connection, table) {
        return 0;
    }
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get::<_, i64>(0)
        })
        .ok()
        .map(|count| count.max(0) as u64)
        .unwrap_or(0)
}

/// Agent-type and status labels only: short tokens, never paths or free text.
fn is_safe_label(value: &str) -> bool {
    let len = value.chars().count();
    (1..=64).contains(&len)
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':'))
        && !value.contains("..")
        && !value.contains(' ')
        && !value.contains('\n')
        && !value.contains('\0')
}

/// Process basenames only (`node`, `cargo`). Rejects paths and free text.
fn is_safe_comm(value: &str) -> bool {
    is_safe_label(value) && !value.contains('/') && !value.contains('\\') && value != "other" // reserved for the folded tail entry
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

        snapshot_sqlite(&source, &destination, DEFAULT_SQLITE_BUSY_BUDGET).unwrap();
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
        snapshot_sqlite(&source, &destination, DEFAULT_SQLITE_BUSY_BUDGET).unwrap();
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
    fn snapshots_sqlite_while_write_transaction_is_held() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("held-write.sqlite");
        let destination = temp.path().join("held-snapshot.sqlite");
        let connection = Connection::open(&source).unwrap();
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        connection
            .execute_batch(
                "CREATE TABLE tracked_sessions (
                    session_id TEXT NOT NULL,
                    display_id TEXT NOT NULL,
                    agent_type TEXT NOT NULL,
                    root_pid INTEGER NOT NULL,
                    root_starttime_ticks INTEGER NOT NULL,
                    first_seen_ms INTEGER NOT NULL,
                    last_seen_ms INTEGER NOT NULL,
                    match_evidence TEXT NOT NULL,
                    match_confidence REAL NOT NULL,
                    session_path TEXT,
                    command TEXT NOT NULL,
                    cwd TEXT,
                    status TEXT NOT NULL,
                    PRIMARY KEY (session_id, root_pid, root_starttime_ticks)
                );
                CREATE TABLE monitor_windows (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id TEXT NOT NULL,
                    root_pid INTEGER NOT NULL,
                    root_starttime_ticks INTEGER NOT NULL,
                    window_start_ms INTEGER NOT NULL,
                    window_end_ms INTEGER NOT NULL,
                    process_count INTEGER NOT NULL,
                    cpu_ms INTEGER NOT NULL,
                    rss_bytes INTEGER NOT NULL,
                    read_bytes INTEGER NOT NULL,
                    write_bytes INTEGER NOT NULL,
                    file_targets INTEGER NOT NULL,
                    network_targets INTEGER NOT NULL DEFAULT 0
                );",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO tracked_sessions VALUES (
                    'sess-1', 'claude:demo', 'claude', 100, 1,
                    1000, 2000, 'proc_fd', 0.9,
                    '/secret/path', 'claude --danger', '/home/user/private', 'active'
                )",
                [],
            )
            .unwrap();
        drop(connection);

        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let (release_sender, release_receiver) = mpsc::sync_channel(1);
        let writer_source = source.clone();
        let writer = thread::spawn(move || {
            let mut connection = Connection::open(writer_source).unwrap();
            connection.busy_timeout(Duration::from_secs(30)).unwrap();
            let transaction = connection.transaction().unwrap();
            transaction
                .execute(
                    "INSERT INTO tracked_sessions VALUES (
                        'sess-2', 'codex:demo', 'codex', 200, 2,
                        1500, 2500, 'cwd', 0.8,
                        '/other/path', 'codex resume', '/tmp/private', 'active'
                    )",
                    [],
                )
                .unwrap();
            ready_sender.send(()).unwrap();
            release_receiver.recv().unwrap();
            transaction.commit().unwrap();
        });

        ready_receiver
            .recv_timeout(Duration::from_secs(15))
            .unwrap();
        snapshot_sqlite(&source, &destination, AGENTSIGHT_SQLITE_BUSY_BUDGET).unwrap();
        release_sender.send(()).unwrap();
        writer.join().unwrap();

        let snapshot = Connection::open(&destination).unwrap();
        let integrity: String = snapshot
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        let tables: Vec<String> = snapshot
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(tables.contains(&"tracked_sessions".to_string()));
        assert!(tables.contains(&"monitor_windows".to_string()));
        let count: i64 = snapshot
            .query_row("SELECT COUNT(*) FROM tracked_sessions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(count >= 1);
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
        config.sources.agentsight_home = Some(temp.path().join("missing-agentsight"));
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

    #[test]
    fn snapshots_rotating_shell_state_before_archival() {
        let temp = TempDir::new().unwrap();
        let codex = temp.path().join("codex");
        let staging = temp.path().join("staging");
        let source = codex.join("shell_snapshots/session.sh");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::create_dir_all(&staging).unwrap();
        fs::write(&source, b"stable shell state").unwrap();

        let mut config = sample_config(temp.path().join("vault"));
        config.sources.agentsight_home = Some(temp.path().join("missing-agentsight"));
        config.sources.claude_home = Some(temp.path().join("missing-claude"));
        config.sources.codex_home = Some(codex);
        config.sources.grok_home = Some(temp.path().join("missing-grok"));
        config.sources.kimi_home = Some(temp.path().join("missing-kimi"));
        config.sources.opencode_share = Some(temp.path().join("missing-opencode-share"));
        config.sources.opencode_state = Some(temp.path().join("missing-opencode-state"));

        let files = prepare_files(&config, &staging).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].logical_path, "codex/shell_snapshots/session.sh");
        assert_ne!(files[0].read_path, source);
        fs::remove_file(source).unwrap();
        assert_eq!(
            fs::read(&files[0].read_path).unwrap(),
            b"stable shell state"
        );
    }

    #[test]
    fn agentsight_snapshots_and_summary_exclude_sensitive_fields() {
        let temp = TempDir::new().unwrap();
        let agentsight = temp.path().join("agentsight");
        let monitor = agentsight.join("monitor");
        let staging = temp.path().join("staging");
        fs::create_dir_all(&monitor).unwrap();
        fs::create_dir_all(&staging).unwrap();
        fs::write(monitor.join("monitor.pid"), b"42").unwrap();
        fs::write(monitor.join("monitor-2026-W25.db-wal"), b"wal").unwrap();

        let db_path = monitor.join("monitor-2026-W25.db");
        let connection = Connection::open(&db_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE tracked_sessions (
                    session_id TEXT NOT NULL,
                    display_id TEXT NOT NULL,
                    agent_type TEXT NOT NULL,
                    root_pid INTEGER NOT NULL,
                    root_starttime_ticks INTEGER NOT NULL,
                    first_seen_ms INTEGER NOT NULL,
                    last_seen_ms INTEGER NOT NULL,
                    match_evidence TEXT NOT NULL,
                    match_confidence REAL NOT NULL,
                    session_path TEXT,
                    command TEXT NOT NULL,
                    cwd TEXT,
                    status TEXT NOT NULL,
                    PRIMARY KEY (session_id, root_pid, root_starttime_ticks)
                );
                CREATE TABLE monitor_windows (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id TEXT NOT NULL,
                    root_pid INTEGER NOT NULL,
                    root_starttime_ticks INTEGER NOT NULL,
                    window_start_ms INTEGER NOT NULL,
                    window_end_ms INTEGER NOT NULL,
                    process_count INTEGER NOT NULL,
                    cpu_ms INTEGER NOT NULL,
                    rss_bytes INTEGER NOT NULL,
                    read_bytes INTEGER NOT NULL,
                    write_bytes INTEGER NOT NULL,
                    file_targets INTEGER NOT NULL,
                    network_targets INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE process_samples (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    window_id INTEGER NOT NULL,
                    rank INTEGER NOT NULL,
                    rank_kind TEXT NOT NULL,
                    pid INTEGER,
                    pid_starttime_ticks INTEGER,
                    ppid INTEGER,
                    depth INTEGER NOT NULL,
                    comm TEXT NOT NULL,
                    command TEXT NOT NULL,
                    cwd TEXT,
                    cpu_ms INTEGER NOT NULL,
                    cpu_percent REAL NOT NULL,
                    rss_bytes INTEGER NOT NULL,
                    read_bytes INTEGER NOT NULL,
                    write_bytes INTEGER NOT NULL,
                    live_count INTEGER NOT NULL
                );
                CREATE TABLE file_samples (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    window_id INTEGER NOT NULL,
                    rank INTEGER NOT NULL,
                    rank_kind TEXT NOT NULL,
                    target TEXT NOT NULL,
                    count INTEGER NOT NULL
                );
                CREATE TABLE network_samples (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    window_id INTEGER NOT NULL,
                    rank INTEGER NOT NULL,
                    rank_kind TEXT NOT NULL,
                    target TEXT NOT NULL,
                    count INTEGER NOT NULL
                );",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO tracked_sessions VALUES (
                    'sess-1', 'claude:/home/user/secret-project', 'claude', 11, 1,
                    1_700_000_000_000, 1_700_000_100_000, 'proc_fd', 0.95,
                    '/home/user/secret-project/.claude',
                    'claude --dangerously-skip-permissions',
                    '/home/user/secret-project', 'active'
                )",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO tracked_sessions VALUES (
                    'sess-2', 'codex:work', 'codex', 22, 2,
                    1_700_000_050_000, 1_700_000_200_000, 'cwd', 0.8,
                    NULL, 'codex', '/tmp/other', 'ended'
                )",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO monitor_windows (
                    session_id, root_pid, root_starttime_ticks, window_start_ms, window_end_ms,
                    process_count, cpu_ms, rss_bytes, read_bytes, write_bytes,
                    file_targets, network_targets
                ) VALUES ('sess-1', 11, 1, 1, 2, 3, 400, 512000, 100, 200, 5, 2)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO process_samples (
                    window_id, rank, rank_kind, depth, comm, command, cwd,
                    cpu_ms, cpu_percent, rss_bytes, read_bytes, write_bytes, live_count
                ) VALUES (1, 1, 'cpu', 0, 'claude', 'claude --secret', '/home/user/secret', 10, 1.0, 1, 1, 1, 1)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO file_samples (window_id, rank, rank_kind, target, count)
                 VALUES (1, 1, 'read', '/home/user/secret/key.pem', 3)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO network_samples (window_id, rank, rank_kind, target, count)
                 VALUES (1, 1, 'connect', 'api.openai.com:443', 7)",
                [],
            )
            .unwrap();
        drop(connection);

        let mut config = sample_config(temp.path().join("vault"));
        config.sources.agentsight_home = Some(agentsight);
        config.sources.claude_home = Some(temp.path().join("missing-claude"));
        config.sources.codex_home = Some(temp.path().join("missing-codex"));
        config.sources.grok_home = Some(temp.path().join("missing-grok"));
        config.sources.kimi_home = Some(temp.path().join("missing-kimi"));
        config.sources.opencode_share = Some(temp.path().join("missing-opencode-share"));
        config.sources.opencode_state = Some(temp.path().join("missing-opencode-state"));

        let files = prepare_files(&config, &staging).unwrap();
        let logical: Vec<_> = files
            .iter()
            .map(|file| file.logical_path.as_str())
            .collect();
        assert_eq!(
            logical,
            vec![
                AGENTSIGHT_ACTIVITY_SUMMARY_PATH,
                "agentsight/monitor/monitor-2026-W25.db"
            ]
        );
        assert!(
            !logical
                .iter()
                .any(|path| path.contains("pid") || path.contains("-wal") || path.contains("-shm"))
        );

        let snapshot = files
            .iter()
            .find(|file| file.logical_path.ends_with("monitor-2026-W25.db"))
            .unwrap();
        assert_ne!(snapshot.read_path, db_path);
        assert_eq!(snapshot.kind, ArchivedFileKind::SqliteSnapshot);
        let opened = Connection::open(&snapshot.read_path).unwrap();
        let sessions: i64 = opened
            .query_row("SELECT COUNT(*) FROM tracked_sessions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(sessions, 2);

        let summary_file = files
            .iter()
            .find(|file| file.logical_path == AGENTSIGHT_ACTIVITY_SUMMARY_PATH)
            .unwrap();
        let summary_text = fs::read_to_string(&summary_file.read_path).unwrap();
        let summary: serde_json::Value = serde_json::from_str(&summary_text).unwrap();
        assert_eq!(summary["schema_version"], 2);
        assert_eq!(summary["totals"]["sessions"], 2);
        assert_eq!(summary["totals"]["by_agent_type"]["claude"], 1);
        assert_eq!(summary["totals"]["by_agent_type"]["codex"], 1);
        assert_eq!(
            summary["databases"][0]["sessions"]["by_status"]["active"],
            1
        );
        assert_eq!(summary["databases"][0]["windows"]["total_cpu_ms"], 400);
        assert_eq!(summary["databases"][0]["samples"]["process_rows"], 1);
        assert_eq!(summary["databases"][0]["schema_recognized"], true);
        assert_eq!(
            summary["databases"][0]["schema_parts"]["tracked_sessions"],
            true
        );
        assert_eq!(
            summary["databases"][0]["schema_parts"]["monitor_windows"],
            true
        );
        assert_eq!(
            summary["databases"][0]["schema_parts"]["process_samples"],
            true
        );
        // Activity series present with stated bucket width.
        assert!(
            summary["databases"][0]["activity_series"]["bucket_width_ms"]
                .as_i64()
                .unwrap()
                > 0
        );
        assert!(
            !summary["databases"][0]["activity_series"]["buckets"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        // Per-session rows: opaque ids, no paths; shape length ≤ target.
        let session_rows = summary["databases"][0]["session_rows"]["rows"]
            .as_array()
            .unwrap();
        assert_eq!(session_rows.len(), 2);
        assert_eq!(
            summary["databases"][0]["session_rows"]["selection"],
            "longest"
        );
        assert_eq!(
            summary["databases"][0]["session_rows"]["cpu_shape_points"],
            16
        );
        for row in session_rows {
            let id = row["id"].as_str().unwrap();
            assert_eq!(id.len(), 8);
            assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
            let shape = row["cpu_shape"].as_array().unwrap();
            // Natural length: this fixture has ≤1 window per session, so shape
            // is at most 1 point — never zero-padded up to the target.
            assert!(shape.len() <= 16);
            assert!(!row.as_object().unwrap().contains_key("display_id"));
            assert!(!row.as_object().unwrap().contains_key("session_path"));
            assert!(!row.as_object().unwrap().contains_key("cwd"));
            assert!(!row.as_object().unwrap().contains_key("command"));
        }
        // Program histogram from process_samples.comm only.
        let programs = summary["databases"][0]["programs"]["entries"]
            .as_array()
            .unwrap();
        assert!(!programs.is_empty());
        assert_eq!(programs[0]["comm"], "claude");
        for forbidden in [
            "/home/user/secret-project",
            "claude --dangerously-skip-permissions",
            "claude --secret",
            "api.openai.com",
            "key.pem",
            "secret-project",
            "display_id",
            "session_path",
            "match_evidence",
            "/home/user/secret",
        ] {
            assert!(
                !summary_text.contains(forbidden),
                "summary leaked sensitive content: {forbidden}\n{summary_text}"
            );
        }
    }

    #[test]
    fn agentsight_schema_recognition_honest_when_columns_missing() {
        let temp = TempDir::new().unwrap();
        let agentsight = temp.path().join("agentsight");
        let monitor = agentsight.join("monitor");
        let staging = temp.path().join("staging");
        fs::create_dir_all(&monitor).unwrap();
        fs::create_dir_all(&staging).unwrap();

        let db_path = monitor.join("monitor-2026-W10.db");
        let connection = Connection::open(&db_path).unwrap();
        // Tables exist but lack the columns required to read real aggregates.
        connection
            .execute_batch(
                "CREATE TABLE tracked_sessions (id INTEGER PRIMARY KEY);
                 CREATE TABLE monitor_windows (id INTEGER PRIMARY KEY);
                 CREATE TABLE process_samples (id INTEGER PRIMARY KEY);",
            )
            .unwrap();
        drop(connection);

        let mut config = sample_config(temp.path().join("vault"));
        config.sources.agentsight_home = Some(agentsight);
        config.sources.claude_home = Some(temp.path().join("missing-claude"));
        config.sources.codex_home = Some(temp.path().join("missing-codex"));
        config.sources.grok_home = Some(temp.path().join("missing-grok"));
        config.sources.kimi_home = Some(temp.path().join("missing-kimi"));
        config.sources.opencode_share = Some(temp.path().join("missing-opencode-share"));
        config.sources.opencode_state = Some(temp.path().join("missing-opencode-state"));

        let files = prepare_files(&config, &staging).unwrap();
        let summary_file = files
            .iter()
            .find(|file| file.logical_path == AGENTSIGHT_ACTIVITY_SUMMARY_PATH)
            .unwrap();
        let summary: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&summary_file.read_path).unwrap()).unwrap();
        assert_eq!(summary["databases"][0]["schema_recognized"], false);
        assert_eq!(
            summary["databases"][0]["schema_parts"]["tracked_sessions"],
            false
        );
        assert_eq!(
            summary["databases"][0]["schema_parts"]["monitor_windows"],
            false
        );
        assert_eq!(
            summary["databases"][0]["schema_parts"]["process_samples"],
            false
        );
        assert!(summary["databases"][0]["activity_series"].is_null());
        assert!(summary["databases"][0]["session_rows"].is_null());
        assert!(summary["databases"][0]["programs"].is_null());
    }

    #[test]
    fn choose_bucket_width_stays_near_target() {
        // One hour of data → 1-minute buckets (under target).
        assert_eq!(choose_bucket_width_ms(3_600_000), 60_000);
        // One week → about 200 buckets at ~50 min → snaps to 1 hour.
        let week = 7 * 24 * 60 * 60_000i64;
        let width = choose_bucket_width_ms(week);
        let buckets = (week + width - 1) / width;
        assert!(buckets <= 200, "buckets={buckets} width={width}");
        assert!(width >= 60_000);
    }

    #[test]
    fn cpu_shape_short_session_is_natural_length_without_padding_zeros() {
        // 12 non-zero windows into a 16-point target must yield 12 points —
        // never 16 with artificial zero gaps between samples.
        let cpu: Vec<i64> = (1..=12).map(|i| 100 + i * 10).collect();
        let shape = build_cpu_shape_from_cpu_ms(&cpu, 16);
        assert_eq!(shape.len(), 12, "expected natural length, got {shape:?}");
        // Every source window was non-zero → no zeros in the shape at all.
        assert!(
            shape.iter().all(|&v| v > 0.0),
            "artificial zeros in short shape: {shape:?}"
        );
        // Peak normalization: max is 1.0 at the true peak (last sample here).
        let peak_idx = shape
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        assert_eq!(peak_idx, 11);
        assert!((shape[peak_idx] - 1.0).abs() < 1e-9);
        // Monotonic source → monotonic shape (no comb).
        for pair in shape.windows(2) {
            assert!(
                pair[0] <= pair[1] + 1e-9,
                "shape should stay monotonic: {shape:?}"
            );
        }
    }

    #[test]
    fn cpu_shape_preserves_genuine_zero_cpu_window() {
        let cpu = [100i64, 0, 50, 0, 200];
        let shape = build_cpu_shape_from_cpu_ms(&cpu, 16);
        assert_eq!(shape.len(), 5);
        assert_eq!(shape[1], 0.0);
        assert_eq!(shape[3], 0.0);
        assert!((shape[4] - 1.0).abs() < 1e-9);
        assert!(shape[0] > 0.0 && shape[2] > 0.0);
    }

    #[test]
    fn cpu_shape_flat_tail_survives_downsampling() {
        // High activity then a sustained flat low tail — the stuck-retry
        // signature. After downsampling to 16 points the tail must stay flat
        // and clearly lower than the active head (not a comb of zeros).
        let mut cpu = vec![800i64; 40];
        cpu.extend(std::iter::repeat_n(30i64, 32));
        assert!(cpu.len() > 16);
        let shape = build_cpu_shape_from_cpu_ms(&cpu, 16);
        assert_eq!(shape.len(), 16);
        // No artificial zeros: every bin got windows with cpu > 0.
        assert!(
            shape.iter().all(|&v| v > 0.0),
            "unexpected zeros after downsample: {shape:?}"
        );
        // With 40 high + 32 low into 16 bins, pure-high bins are early and
        // pure-low bins are late (boundary bin may straddle the transition).
        let head: f64 = shape[..8].iter().sum::<f64>() / 8.0;
        let tail: f64 = shape[10..].iter().sum::<f64>() / 6.0;
        assert!(
            head > tail * 5.0,
            "flat tail not visible after downsample: head={head} tail={tail} shape={shape:?}"
        );
        // Tail points should be nearly equal (flat), not spiky.
        let tail_min = shape[10..].iter().cloned().fold(f64::INFINITY, f64::min);
        let tail_max = shape[10..].iter().cloned().fold(0.0f64, f64::max);
        assert!(tail_max - tail_min < 0.05, "tail not flat: {shape:?}");
        // Peak is in the active head, not forced onto the last sample.
        let peak_idx = shape
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        assert!(
            peak_idx < 9,
            "peak should be in active head, got idx={peak_idx} shape={shape:?}"
        );
    }

    #[test]
    fn cpu_shape_peak_is_true_max_not_last_bin_bias() {
        // Peak in the middle; last sample is medium. After downsampling the
        // normalized 1.0 must land on the mid peak, not the final bin.
        let mut cpu = vec![10i64; 32];
        cpu[15] = 500;
        cpu[31] = 50;
        let shape = build_cpu_shape_from_cpu_ms(&cpu, 16);
        assert_eq!(shape.len(), 16);
        let peak_idx = shape
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        assert!((shape[peak_idx] - 1.0).abs() < 1e-9);
        assert_ne!(peak_idx, 15, "peak should not be last bin: {shape:?}");
        // Window 15 of 32 → bin floor(15*16/32)=7 or in equal partition
        // start..end for j covering index 15.
        assert!(
            (6..=8).contains(&peak_idx),
            "mid peak expected near bin 7, got {peak_idx}: {shape:?}"
        );
    }

    #[test]
    fn agentsight_skips_invalid_sqlite_without_failing_commit() {
        let temp = TempDir::new().unwrap();
        let agentsight = temp.path().join("agentsight");
        let monitor = agentsight.join("monitor");
        let staging = temp.path().join("staging");
        fs::create_dir_all(&monitor).unwrap();
        fs::create_dir_all(&staging).unwrap();
        fs::write(monitor.join("monitor-2026-W01.db"), b"not a database").unwrap();

        let mut config = sample_config(temp.path().join("vault"));
        config.sources.agentsight_home = Some(agentsight);
        config.sources.claude_home = Some(temp.path().join("missing-claude"));
        config.sources.codex_home = Some(temp.path().join("missing-codex"));
        config.sources.grok_home = Some(temp.path().join("missing-grok"));
        config.sources.kimi_home = Some(temp.path().join("missing-kimi"));
        config.sources.opencode_share = Some(temp.path().join("missing-opencode-share"));
        config.sources.opencode_state = Some(temp.path().join("missing-opencode-state"));

        let files = prepare_files(&config, &staging).unwrap();
        assert!(files.is_empty());
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
