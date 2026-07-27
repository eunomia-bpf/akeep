use std::collections::HashSet;
use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use rusqlite::{Connection, OpenFlags, Statement, params};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::config::Config;
use crate::manifest::{ArchivedFileKind, FileRecord};
use crate::providers::Provider;
use crate::vault::Vault;

const INDEX_FORMAT_VERSION: u32 = 1;
const MAX_INDEX_LINE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IndexReport {
    pub index_path: PathBuf,
    pub indexed_at: String,
    pub recovery_points_scanned: u64,
    pub files: u64,
    pub lines: u64,
    pub logical_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SearchResult {
    pub provider: Provider,
    pub logical_path: String,
    pub snapshot_id: String,
    pub line_number: u64,
    pub snippet: String,
    pub score: f64,
}

pub fn rebuild(config: &Config) -> Result<IndexReport> {
    let vault = Vault::open(config)?;
    let mut manifests = vault.list_manifests()?;
    for manifest in &manifests {
        manifest.validate(config.vault.id)?;
    }
    manifests.sort_by_key(|manifest| std::cmp::Reverse(manifest.created_at));
    if manifests.is_empty() {
        bail!("no recovery points are available to index");
    }

    let index_path = index_path(config);
    let parent = index_path
        .parent()
        .with_context(|| format!("index path {} has no parent", index_path.display()))?;
    crate::config::create_private_directory(parent)?;
    let temporary =
        NamedTempFile::new_in(parent).context("failed to create temporary search index")?;
    set_private_file(&temporary)?;

    let mut connection =
        Connection::open(temporary.path()).context("failed to create SQLite search index")?;
    connection.execute_batch(
        "PRAGMA journal_mode = DELETE;
         PRAGMA synchronous = FULL;
         CREATE TABLE metadata (
             key TEXT PRIMARY KEY NOT NULL,
             value TEXT NOT NULL
         ) STRICT;
         CREATE VIRTUAL TABLE documents USING fts5(
             body,
             provider UNINDEXED,
             logical_path UNINDEXED,
             snapshot_id UNINDEXED,
             line_number UNINDEXED,
             tokenize = 'unicode61 remove_diacritics 2'
         );",
    )?;
    let indexed_at = Utc::now().to_rfc3339();
    connection.execute(
        "INSERT INTO metadata (key, value) VALUES
             ('format_version', ?1),
             ('vault_id', ?2),
             ('indexed_at', ?3)",
        params![
            INDEX_FORMAT_VERSION.to_string(),
            config.vault.id.to_string(),
            indexed_at
        ],
    )?;

    let transaction = connection.transaction()?;
    let mut insert = transaction.prepare(
        "INSERT INTO documents
             (body, provider, logical_path, snapshot_id, line_number)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    let mut seen_paths = HashSet::new();
    let mut files = 0_u64;
    let mut lines = 0_u64;
    let mut logical_bytes = 0_u64;

    for manifest in &manifests {
        for file in &manifest.files {
            if !matches!(file.provider, Provider::ClaudeCode | Provider::Codex)
                || !is_indexable(file)
                || !seen_paths.insert((file.provider, file.logical_path.clone()))
            {
                continue;
            }
            let indexed_lines = index_file(&vault, file, &manifest.snapshot_id, &mut insert)?;
            files += 1;
            lines = lines
                .checked_add(indexed_lines)
                .context("indexed line count overflow")?;
            logical_bytes = logical_bytes
                .checked_add(file.size)
                .context("indexed byte count overflow")?;
        }
    }
    drop(insert);
    transaction.commit()?;
    connection.execute_batch("INSERT INTO documents(documents) VALUES('optimize');")?;
    drop(connection);
    temporary.as_file().sync_all()?;
    temporary
        .persist(&index_path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to publish search index {}", index_path.display()))?;
    sync_directory(parent)?;

    Ok(IndexReport {
        index_path,
        indexed_at,
        recovery_points_scanned: manifests.len() as u64,
        files,
        lines,
        logical_bytes,
    })
}

pub fn query(config: &Config, query: &str, limit: u32) -> Result<Vec<SearchResult>> {
    if query.trim().is_empty() {
        bail!("search query must not be empty");
    }
    if !(1..=1000).contains(&limit) {
        bail!("search limit must be between 1 and 1000");
    }
    let index_path = index_path(config);
    if !index_path.is_file() {
        bail!(
            "search index does not exist at {}; run `akeep index rebuild`",
            index_path.display()
        );
    }
    let connection = Connection::open_with_flags(
        &index_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("failed to open search index {}", index_path.display()))?;
    validate_index(&connection, config)?;

    let expression = literal_fts_expression(query)?;
    let mut statement = connection.prepare(
        "SELECT provider,
                logical_path,
                snapshot_id,
                CAST(line_number AS INTEGER),
                snippet(documents, 0, '[', ']', ' … ', 24),
                bm25(documents)
         FROM documents
         WHERE documents MATCH ?1
         ORDER BY bm25(documents), logical_path, CAST(line_number AS INTEGER)
         LIMIT ?2",
    )?;
    let rows = statement.query_map(params![expression, limit], |row| {
        let provider: String = row.get(0)?;
        Ok((
            provider,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, u64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, f64>(5)?,
        ))
    })?;

    rows.map(|row| {
        let (provider, logical_path, snapshot_id, line_number, snippet, score) = row?;
        Ok(SearchResult {
            provider: parse_provider(&provider)?,
            logical_path,
            snapshot_id,
            line_number,
            snippet,
            score,
        })
    })
    .collect()
}

fn index_file(
    vault: &Vault,
    file: &FileRecord,
    snapshot_id: &str,
    insert: &mut Statement<'_>,
) -> Result<u64> {
    let mut pending = Vec::new();
    let mut line_number = 1_u64;
    let mut inserted = 0_u64;
    vault.visit_file_chunks(file, |chunk| {
        for byte in chunk {
            pending.push(*byte);
            if *byte == b'\n' || pending.len() >= MAX_INDEX_LINE_BYTES {
                inserted += insert_line(insert, file, snapshot_id, line_number, &mut pending)?;
                if *byte == b'\n' {
                    line_number += 1;
                }
            }
        }
        Ok(())
    })?;
    if !pending.is_empty() {
        inserted += insert_line(insert, file, snapshot_id, line_number, &mut pending)?;
    }
    Ok(inserted)
}

fn insert_line(
    insert: &mut Statement<'_>,
    file: &FileRecord,
    snapshot_id: &str,
    line_number: u64,
    pending: &mut Vec<u8>,
) -> Result<u64> {
    while matches!(pending.last(), Some(b'\n' | b'\r')) {
        pending.pop();
    }
    if pending.iter().all(u8::is_ascii_whitespace) {
        pending.clear();
        return Ok(0);
    }
    let body = String::from_utf8_lossy(pending);
    insert.execute(params![
        body.as_ref(),
        file.provider.id(),
        file.logical_path,
        snapshot_id,
        line_number
    ])?;
    pending.clear();
    Ok(1)
}

fn is_indexable(file: &FileRecord) -> bool {
    if file.kind == ArchivedFileKind::SqliteSnapshot {
        return false;
    }
    let extension = Path::new(&file.logical_path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    matches!(
        extension.as_deref(),
        Some("json" | "jsonl" | "md" | "txt" | "log")
    )
}

fn validate_index(connection: &Connection, config: &Config) -> Result<()> {
    let lookup = |key: &str| -> Result<String> {
        connection
            .query_row("SELECT value FROM metadata WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .with_context(|| format!("search index is missing metadata key {key}"))
    };
    let version = lookup("format_version")?;
    if version != INDEX_FORMAT_VERSION.to_string() {
        bail!("unsupported search index format {version}; rebuild the index");
    }
    let vault_id = lookup("vault_id")?;
    if vault_id != config.vault.id.to_string() {
        bail!("search index belongs to another vault; rebuild the index");
    }
    Ok(())
}

fn literal_fts_expression(query: &str) -> Result<String> {
    let terms = query
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    if terms.is_empty() {
        bail!("search query must contain a non-whitespace term");
    }
    Ok(terms.join(" AND "))
}

fn parse_provider(value: &str) -> Result<Provider> {
    Provider::ALL
        .into_iter()
        .find(|provider| provider.id() == value)
        .with_context(|| format!("search index contains unknown provider {value:?}"))
}

fn index_path(config: &Config) -> PathBuf {
    config.vault.state_path.join("search.sqlite3")
}

fn set_private_file(file: &NamedTempFile) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
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
    use super::*;

    #[test]
    fn quotes_user_input_as_literal_terms() {
        assert_eq!(
            literal_fts_expression("hello OR \"world\"").unwrap(),
            "\"hello\" AND \"OR\" AND \"\"\"world\"\"\""
        );
    }
}
