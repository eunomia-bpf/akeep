use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::engine::general_purpose::STANDARD;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::manifest::{ArchivedFileKind, FileRecord};
use crate::vault::Vault;

const MAX_MARKDOWN_FILE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum ExportFormat {
    Markdown,
    Json,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExportReport {
    pub snapshot_id: String,
    pub format: ExportFormat,
    pub output: PathBuf,
    pub files_included: u64,
    pub files_omitted: u64,
    pub logical_bytes_included: u64,
}

pub fn export(
    config: &Config,
    reference: &str,
    format: ExportFormat,
    output_path: &Path,
) -> Result<ExportReport> {
    let vault = Vault::open(config)?;
    let manifest = vault.load_manifest(reference)?;
    manifest.validate(config.vault.id)?;
    let mut output = create_output(&vault, output_path)?;

    let write_result = (|| -> Result<(u64, u64, u64)> {
        let counts = match format {
            ExportFormat::Json => write_json(&vault, &manifest, &mut output)?,
            ExportFormat::Markdown => write_markdown(&vault, &manifest, &mut output)?,
        };
        output
            .sync_all()
            .with_context(|| format!("failed to sync export {}", output_path.display()))?;
        Ok(counts)
    })();
    let (files_included, files_omitted, logical_bytes_included) = match write_result {
        Ok(counts) => counts,
        Err(error) => {
            drop(output);
            let _ = fs::remove_file(output_path);
            return Err(error);
        }
    };
    let parent = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    sync_directory(parent)?;

    Ok(ExportReport {
        snapshot_id: manifest.snapshot_id,
        format,
        output: fs::canonicalize(output_path).unwrap_or_else(|_| output_path.to_path_buf()),
        files_included,
        files_omitted,
        logical_bytes_included,
    })
}

fn write_json(
    vault: &Vault,
    manifest: &crate::manifest::Manifest,
    output: &mut File,
) -> Result<(u64, u64, u64)> {
    output.write_all(b"{\"export_format_version\":1,\"manifest\":")?;
    serde_json::to_writer(&mut *output, manifest)?;
    output.write_all(b",\"files\":[")?;
    for (index, record) in manifest.files.iter().enumerate() {
        if index > 0 {
            output.write_all(b",")?;
        }
        output.write_all(b"{\"metadata\":")?;
        serde_json::to_writer(&mut *output, record)?;
        output.write_all(b",\"content_encoding\":\"base64\",\"content\":\"")?;
        {
            let mut encoder = base64::write::EncoderWriter::new(&mut *output, &STANDARD);
            vault.visit_file_chunks(record, |chunk| {
                encoder
                    .write_all(chunk)
                    .context("failed to write base64 export")
            })?;
            encoder.finish()?;
        }
        output.write_all(b"\"}")?;
    }
    output.write_all(b"]}\n")?;
    Ok((
        manifest.files.len() as u64,
        0,
        manifest.files.iter().map(|file| file.size).sum(),
    ))
}

fn write_markdown(
    vault: &Vault,
    manifest: &crate::manifest::Manifest,
    output: &mut File,
) -> Result<(u64, u64, u64)> {
    writeln!(output, "# Akeep export\n")?;
    writeln!(output, "- Recovery point: `{}`", manifest.snapshot_id)?;
    writeln!(output, "- Created: `{}`", manifest.created_at.to_rfc3339())?;
    writeln!(output, "- Host: `{}`", escape_inline(&manifest.hostname))?;
    writeln!(output, "- Files in manifest: `{}`\n", manifest.files.len())?;
    writeln!(
        output,
        "> This is a readable view. Use JSON export or `akeep recover` for an exact representation.\n"
    )?;

    let mut included = 0_u64;
    let mut omitted = 0_u64;
    let mut included_bytes = 0_u64;
    for record in &manifest.files {
        writeln!(
            output,
            "## <code>{}</code>\n",
            escape_inline(&record.logical_path)
        )?;
        writeln!(
            output,
            "Provider: `{}` · Bytes: `{}` · BLAKE3: `{}`\n",
            record.provider, record.size, record.blake3
        )?;
        if !is_markdown_text(record) {
            writeln!(
                output,
                "_Binary or non-text artifact omitted from this readable export._\n"
            )?;
            omitted += 1;
            continue;
        }
        if record.size > MAX_MARKDOWN_FILE_BYTES {
            writeln!(
                output,
                "_Text artifact exceeds the 64 MiB readable-export limit; use JSON export or recovery._\n"
            )?;
            omitted += 1;
            continue;
        }
        let contents = vault.read_file(record)?;
        let Ok(text) = std::str::from_utf8(&contents) else {
            writeln!(
                output,
                "_Artifact is not valid UTF-8; use JSON export or recovery._\n"
            )?;
            omitted += 1;
            continue;
        };
        let fence = markdown_fence(text);
        writeln!(output, "{fence}text")?;
        output.write_all(text.as_bytes())?;
        if !text.ends_with('\n') {
            writeln!(output)?;
        }
        writeln!(output, "{fence}\n")?;
        included += 1;
        included_bytes = included_bytes
            .checked_add(record.size)
            .context("export byte count overflow")?;
    }
    Ok((included, omitted, included_bytes))
}

pub(crate) fn create_output(vault: &Vault, path: &Path) -> Result<File> {
    if path.file_name().is_none() {
        bail!("export output must name a file: {}", path.display());
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let resolved_parent = fs::canonicalize(parent)
        .with_context(|| format!("export parent does not exist: {}", parent.display()))?;
    let resolved_output = resolved_parent.join(path.file_name().unwrap());
    for protected in vault
        .filesystem_root()
        .into_iter()
        .chain(std::iter::once(vault.state_root()))
    {
        let protected = fs::canonicalize(protected)
            .with_context(|| format!("failed to resolve Akeep path {}", protected.display()))?;
        if paths_overlap(&protected, &resolved_output) {
            bail!(
                "export output {} overlaps vault/state path {}",
                path.display(),
                protected.display()
            );
        }
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("refusing to overwrite export {}", path.display()))
}

fn is_markdown_text(file: &FileRecord) -> bool {
    if file.kind == ArchivedFileKind::SqliteSnapshot {
        return false;
    }
    let extension = Path::new(&file.logical_path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    matches!(
        extension.as_deref(),
        Some("json" | "jsonl" | "md" | "txt" | "log" | "toml" | "yaml" | "yml")
    )
}

fn markdown_fence(text: &str) -> String {
    let mut longest = 0;
    let mut current = 0;
    for character in text.chars() {
        if character == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    "`".repeat((longest + 1).max(3))
}

fn escape_inline(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
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
    use base64::Engine;

    use super::*;

    #[test]
    fn fence_is_longer_than_embedded_backticks() {
        assert_eq!(markdown_fence("plain"), "```");
        assert_eq!(markdown_fence("```` inside"), "`````");
    }

    #[test]
    fn standard_base64_has_no_json_string_escapes() {
        let encoded = STANDARD.encode([0, 1, 2, 250, 251, 252]);
        assert!(!encoded.contains(['"', '\\']));
    }
}
