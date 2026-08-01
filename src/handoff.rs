use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::manifest::{ArchivedFileKind, FileRecord};
use crate::providers::Provider;
use crate::vault::Vault;

const MAX_CONTEXT_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CONTEXT_TAIL_BYTES: usize = 32 * 1024;
const MAX_CONTEXT_FILES: usize = 3;

#[derive(Clone, Debug)]
pub struct HandoffRequest {
    pub snapshot: String,
    pub from: Provider,
    pub for_agent: Provider,
    pub goal: String,
    pub decisions: Vec<String>,
    pub open_tasks: Vec<String>,
    pub test_status: Vec<String>,
    pub artifacts: Vec<PathBuf>,
    pub repository: PathBuf,
    pub output: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HandoffReport {
    pub snapshot_id: String,
    pub from: Provider,
    pub for_agent: Provider,
    pub repository: PathBuf,
    pub output: PathBuf,
    pub changed_files: u64,
    pub artifacts: u64,
    pub context_files: u64,
}

struct RepositoryState {
    root: PathBuf,
    branch: String,
    commit: String,
    status: String,
    worktree_diff_stat: String,
    staged_diff_stat: String,
    changed_files: u64,
}

struct Artifact {
    path: String,
    bytes: u64,
    blake3: String,
}

pub fn create(config: &Config, request: &HandoffRequest) -> Result<HandoffReport> {
    validate_route(request.from, request.for_agent)?;
    if request.goal.trim().is_empty() {
        bail!("handoff goal must not be empty");
    }
    let repository = inspect_repository(&request.repository)?;
    let artifacts = inspect_artifacts(&repository.root, &request.artifacts)?;
    let vault = Vault::open(config)?;
    let manifest = vault.load_manifest(&request.snapshot)?;
    manifest.validate(config.vault.id)?;
    let context = collect_context(&vault, &manifest.files, request.from)?;

    let markdown = render(
        request,
        &manifest.snapshot_id,
        &repository,
        &artifacts,
        &context,
    );
    let mut output = create_output(&vault, &request.output)?;
    let write_result = (|| -> Result<()> {
        output
            .write_all(markdown.as_bytes())
            .with_context(|| format!("failed to write handoff {}", request.output.display()))?;
        output
            .sync_all()
            .with_context(|| format!("failed to sync handoff {}", request.output.display()))?;
        let parent = request
            .output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        sync_directory(parent)?;
        Ok(())
    })();
    if let Err(error) = write_result {
        drop(output);
        let _ = fs::remove_file(&request.output);
        return Err(error);
    }

    Ok(HandoffReport {
        snapshot_id: manifest.snapshot_id,
        from: request.from,
        for_agent: request.for_agent,
        repository: repository.root,
        output: fs::canonicalize(&request.output).unwrap_or_else(|_| request.output.clone()),
        changed_files: repository.changed_files,
        artifacts: artifacts.len() as u64,
        context_files: context.len() as u64,
    })
}

fn validate_route(from: Provider, for_agent: Provider) -> Result<()> {
    let supported = |provider| matches!(provider, Provider::ClaudeCode | Provider::Codex);
    if !supported(from) || !supported(for_agent) {
        bail!("semantic handoff currently supports Claude Code and Codex");
    }
    if from == for_agent {
        bail!("handoff source and destination agents must differ");
    }
    Ok(())
}

fn inspect_repository(path: &Path) -> Result<RepositoryState> {
    let root = git(path, &["rev-parse", "--show-toplevel"])?;
    let root = fs::canonicalize(root.trim())
        .with_context(|| format!("failed to resolve Git repository {}", root.trim()))?;
    let branch = git(&root, &["branch", "--show-current"])?;
    let commit = git(&root, &["rev-parse", "HEAD"])?;
    let status = git(&root, &["status", "--short"])?;
    let worktree_diff_stat = git(&root, &["diff", "--stat"])?;
    let staged_diff_stat = git(&root, &["diff", "--cached", "--stat"])?;
    let changed_files = status
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count() as u64;
    Ok(RepositoryState {
        root,
        branch: if branch.trim().is_empty() {
            "(detached)".to_string()
        } else {
            branch.trim().to_string()
        },
        commit: commit.trim().to_string(),
        status: status.trim_end().to_string(),
        worktree_diff_stat: worktree_diff_stat.trim_end().to_string(),
        staged_diff_stat: staged_diff_stat.trim_end().to_string(),
        changed_files,
    })
}

fn inspect_artifacts(repository: &Path, paths: &[PathBuf]) -> Result<Vec<Artifact>> {
    paths
        .iter()
        .map(|path| {
            let path = if path.is_absolute() {
                path.clone()
            } else {
                repository.join(path)
            };
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("failed to inspect artifact {}", path.display()))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!(
                    "handoff artifact must be a regular file: {}",
                    path.display()
                );
            }
            let canonical = fs::canonicalize(&path)
                .with_context(|| format!("failed to resolve artifact {}", path.display()))?;
            let relative = canonical.strip_prefix(repository).with_context(|| {
                format!(
                    "handoff artifact {} is outside repository {}",
                    canonical.display(),
                    repository.display()
                )
            })?;
            let mut file = File::open(&canonical)?;
            let mut hasher = blake3::Hasher::new();
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            Ok(Artifact {
                path: relative.to_string_lossy().into_owned(),
                bytes: metadata.len(),
                blake3: hasher.finalize().to_hex().to_string(),
            })
        })
        .collect()
}

fn collect_context(
    vault: &Vault,
    files: &[FileRecord],
    provider: Provider,
) -> Result<Vec<(String, String)>> {
    let mut candidates = files
        .iter()
        .filter(|file| {
            file.provider == provider
                && file.kind != ArchivedFileKind::SqliteSnapshot
                && file.size <= MAX_CONTEXT_FILE_BYTES
                && is_text(file)
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .modified_unix_seconds
            .cmp(&left.modified_unix_seconds)
            .then_with(|| right.logical_path.cmp(&left.logical_path))
    });
    candidates
        .into_iter()
        .take(MAX_CONTEXT_FILES)
        .map(|file| {
            let contents = vault.read_file(file)?;
            let start = contents.len().saturating_sub(MAX_CONTEXT_TAIL_BYTES);
            let tail = String::from_utf8_lossy(&contents[start..]).into_owned();
            Ok((file.logical_path.clone(), tail))
        })
        .collect()
}

fn render(
    request: &HandoffRequest,
    snapshot_id: &str,
    repository: &RepositoryState,
    artifacts: &[Artifact],
    context: &[(String, String)],
) -> String {
    let mut output = String::new();
    output.push_str("# Akeep semantic handoff\n\n");
    output.push_str(&format!(
        "- From: `{}`\n- For: `{}`\n- Commit: `{snapshot_id}`\n- Repository: `{}`\n\n",
        request.from,
        request.for_agent,
        repository.root.display()
    ));
    output.push_str("> This bundle is local and reviewable. User-supplied statements are separated from automatically captured repository and archived-session evidence.\n\n");
    output.push_str("## Goal (user supplied)\n\n");
    output.push_str(request.goal.trim());
    output.push_str("\n\n");
    render_list(&mut output, "Decisions (user supplied)", &request.decisions);
    render_list(
        &mut output,
        "Open tasks (user supplied)",
        &request.open_tasks,
    );
    render_list(
        &mut output,
        "Test status (user supplied)",
        &request.test_status,
    );

    output.push_str("## Repository state (captured)\n\n");
    output.push_str(&format!(
        "- Branch: `{}`\n- Commit: `{}`\n- Changed paths: `{}`\n\n",
        repository.branch, repository.commit, repository.changed_files
    ));
    render_code_block(&mut output, "git status --short", &repository.status);
    render_code_block(
        &mut output,
        "git diff --stat",
        &repository.worktree_diff_stat,
    );
    render_code_block(
        &mut output,
        "git diff --cached --stat",
        &repository.staged_diff_stat,
    );

    output.push_str("## Artifacts (captured)\n\n");
    if artifacts.is_empty() {
        output.push_str("- None supplied.\n\n");
    } else {
        for artifact in artifacts {
            output.push_str(&format!(
                "- `{}` — {} bytes — BLAKE3 `{}`\n",
                artifact.path, artifact.bytes, artifact.blake3
            ));
        }
        output.push('\n');
    }

    output.push_str("## Recent archived session context (captured)\n\n");
    output.push_str("These are bounded transcript tails, not an AI summary. Review them for commands, results, assumptions, and unresolved errors.\n\n");
    if context.is_empty() {
        output.push_str("_No eligible text session artifacts were found._\n");
    } else {
        for (path, text) in context {
            output.push_str(&format!("### `{path}`\n\n"));
            let fence = markdown_fence(text);
            output.push_str(&format!("{fence}text\n{text}"));
            if !text.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(&format!("{fence}\n\n"));
        }
    }
    output
}

fn render_list(output: &mut String, title: &str, entries: &[String]) {
    output.push_str(&format!("## {title}\n\n"));
    if entries.is_empty() {
        output.push_str("- Not supplied.\n\n");
    } else {
        for entry in entries {
            output.push_str(&format!("- {}\n", entry.trim()));
        }
        output.push('\n');
    }
}

fn render_code_block(output: &mut String, title: &str, contents: &str) {
    output.push_str(&format!("### `{title}`\n\n"));
    let displayed = if contents.is_empty() {
        "(no output)"
    } else {
        contents
    };
    let fence = markdown_fence(displayed);
    output.push_str(&format!("{fence}text\n{displayed}\n{fence}\n\n"));
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

fn is_text(file: &FileRecord) -> bool {
    let extension = Path::new(&file.logical_path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    matches!(
        extension.as_deref(),
        Some("json" | "jsonl" | "md" | "txt" | "log")
    )
}

fn git(repository: &Path, arguments: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .with_context(|| format!("failed to run git in {}", repository.display()))?;
    if !output.status.success() {
        bail!(
            "git {} failed in {}: {}",
            arguments.join(" "),
            repository.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("git output is not UTF-8")
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)
        .with_context(|| format!("failed to open directory {}", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync directory {}", path.display()))?;
    Ok(())
}

fn create_output(vault: &Vault, path: &Path) -> Result<File> {
    if path.file_name().is_none() {
        bail!("handoff output must name a file: {}", path.display());
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let resolved_parent = fs::canonicalize(parent)
        .with_context(|| format!("handoff parent does not exist: {}", parent.display()))?;
    let resolved_output = resolved_parent.join(path.file_name().unwrap());
    for protected in vault
        .filesystem_root()
        .into_iter()
        .chain(std::iter::once(vault.state_root()))
    {
        let protected = fs::canonicalize(protected)
            .with_context(|| format!("failed to resolve Akeep path {}", protected.display()))?;
        if protected.starts_with(&resolved_output) || resolved_output.starts_with(&protected) {
            bail!(
                "handoff output {} overlaps repository/state path {}",
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
        .with_context(|| format!("refusing to overwrite handoff {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_cross_agent_claude_codex_routes_are_supported() {
        assert!(validate_route(Provider::Codex, Provider::ClaudeCode).is_ok());
        assert!(validate_route(Provider::ClaudeCode, Provider::Codex).is_ok());
        assert!(validate_route(Provider::Codex, Provider::Codex).is_err());
        assert!(validate_route(Provider::Grok, Provider::Codex).is_err());
    }
}
