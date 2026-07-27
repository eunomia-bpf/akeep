use std::env;
use std::fmt;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::ValueEnum;
use directories::BaseDirs;
use serde::{Deserialize, Serialize};

use crate::config::Config;

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum Provider {
    ClaudeCode,
    Codex,
    Grok,
    KimiCode,
    OpenCode,
}

impl Provider {
    pub const ALL: [Self; 5] = [
        Self::ClaudeCode,
        Self::Codex,
        Self::Grok,
        Self::KimiCode,
        Self::OpenCode,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Grok => "grok",
            Self::KimiCode => "kimi-code",
            Self::OpenCode => "opencode",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex CLI",
            Self::Grok => "Grok CLI",
            Self::KimiCode => "Kimi Code",
            Self::OpenCode => "OpenCode",
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    Directory,
    File,
    Sqlite,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceItem {
    pub provider: Provider,
    pub kind: SourceKind,
    pub source_path: PathBuf,
    pub logical_path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderSpec {
    pub provider: Provider,
    pub roots: Vec<PathBuf>,
    pub items: Vec<SourceItem>,
    pub excluded: Vec<PathBuf>,
}

pub fn specifications(config: &Config) -> Result<Vec<ProviderSpec>> {
    let base = BaseDirs::new().context("could not determine the user home directory")?;
    let home = base.home_dir();
    let xdg_data = base.data_local_dir();
    let xdg_state = state_home(home);

    let claude = configured_path(
        config.sources.claude_home.as_ref(),
        "CLAUDE_HOME",
        home.join(".claude"),
    );
    let codex = configured_path(
        config.sources.codex_home.as_ref(),
        "CODEX_HOME",
        home.join(".codex"),
    );
    let grok = configured_path(
        config.sources.grok_home.as_ref(),
        "GROK_HOME",
        home.join(".grok"),
    );
    let kimi = configured_path(
        config.sources.kimi_home.as_ref(),
        "KIMI_CODE_HOME",
        home.join(".kimi-code"),
    );
    let opencode_share = configured_path(
        config.sources.opencode_share.as_ref(),
        "OPENCODE_SHARE_DIR",
        xdg_data.join("opencode"),
    );
    let opencode_state = configured_path(
        config.sources.opencode_state.as_ref(),
        "OPENCODE_STATE_DIR",
        xdg_state.join("opencode"),
    );

    Ok(vec![
        claude_spec(claude),
        codex_spec(codex),
        grok_spec(grok),
        kimi_spec(kimi),
        opencode_spec(opencode_share, opencode_state),
    ])
}

fn configured_path(override_path: Option<&PathBuf>, env_name: &str, fallback: PathBuf) -> PathBuf {
    override_path
        .cloned()
        .or_else(|| env::var_os(env_name).map(PathBuf::from))
        .unwrap_or(fallback)
}

fn state_home(home: &std::path::Path) -> PathBuf {
    env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local").join("state"))
}

fn item(
    provider: Provider,
    root: &std::path::Path,
    relative: &str,
    kind: SourceKind,
) -> SourceItem {
    SourceItem {
        provider,
        kind,
        source_path: root.join(relative),
        logical_path: format!("{}/{relative}", provider.id()),
    }
}

fn claude_spec(root: PathBuf) -> ProviderSpec {
    let provider = Provider::ClaudeCode;
    let mut items = Vec::new();
    for relative in [
        "projects",
        "sessions",
        "file-history",
        "plans",
        "commands",
        "tasks",
        "todos",
        "shell-snapshots",
        "session-env",
        "backups",
    ] {
        items.push(item(provider, &root, relative, SourceKind::Directory));
    }
    items.push(item(provider, &root, "history.jsonl", SourceKind::File));

    ProviderSpec {
        provider,
        roots: vec![root.clone()],
        items,
        excluded: paths(
            &root,
            &[
                ".credentials.json",
                "cache",
                "daemon",
                "debug",
                "downloads",
                "ide",
                "jobs",
                "paste-cache",
                "plugins",
                "settings.json",
                "settings.local.json",
                "statsig",
                "telemetry",
                "usage-data",
            ],
        ),
    }
}

fn codex_spec(root: PathBuf) -> ProviderSpec {
    let provider = Provider::Codex;
    let mut items = Vec::new();
    for relative in [
        "sessions",
        "attachments",
        "memories",
        "shell_snapshots",
        "automations",
    ] {
        items.push(item(provider, &root, relative, SourceKind::Directory));
    }
    for relative in ["history.jsonl", "session_index.jsonl"] {
        items.push(item(provider, &root, relative, SourceKind::File));
    }
    for relative in [
        "state_5.sqlite",
        "logs_2.sqlite",
        "goals_1.sqlite",
        "memories_1.sqlite",
    ] {
        items.push(item(provider, &root, relative, SourceKind::Sqlite));
    }

    ProviderSpec {
        provider,
        roots: vec![root.clone()],
        items,
        excluded: paths(
            &root,
            &[
                ".tmp",
                "app-server-control",
                "app-server-daemon",
                "auth.json",
                "cache",
                "config.toml",
                "log",
                "mcp-oauth-locks",
                "packages",
                "plugins",
                "research-keys",
                "tmp",
                "worktrees",
            ],
        ),
    }
}

fn grok_spec(root: PathBuf) -> ProviderSpec {
    let provider = Provider::Grok;
    let items = vec![
        item(provider, &root, "sessions", SourceKind::Directory),
        item(provider, &root, "active_sessions.json", SourceKind::File),
        item(provider, &root, "worktrees.db", SourceKind::Sqlite),
    ];
    ProviderSpec {
        provider,
        roots: vec![root.clone()],
        items,
        excluded: paths(
            &root,
            &[
                "auth.json",
                "auth.json.lock",
                "bin",
                "bundled",
                "downloads",
                "logs",
                "marketplace-cache",
                "memtrace",
                "relocations",
                "skills",
                "vendor",
            ],
        ),
    }
}

fn kimi_spec(root: PathBuf) -> ProviderSpec {
    let provider = Provider::KimiCode;
    let items = vec![
        item(provider, &root, "sessions", SourceKind::Directory),
        item(provider, &root, "user-history", SourceKind::Directory),
        item(provider, &root, "session_index.jsonl", SourceKind::File),
        item(provider, &root, "workspaces.json", SourceKind::File),
    ];
    ProviderSpec {
        provider,
        roots: vec![root.clone()],
        items,
        excluded: paths(
            &root,
            &[
                "credentials",
                "device_id",
                "logs",
                "oauth",
                "telemetry",
                "updates",
            ],
        ),
    }
}

fn opencode_spec(share: PathBuf, state: PathBuf) -> ProviderSpec {
    let provider = Provider::OpenCode;
    let items = vec![
        item(provider, &share, "opencode.db", SourceKind::Sqlite),
        item(provider, &share, "storage", SourceKind::Directory),
        item(provider, &state, "prompt-history.jsonl", SourceKind::File),
    ];
    let mut excluded = paths(
        &share,
        &[
            "auth.json",
            "bin",
            "log",
            "repos",
            "snapshot",
            "tool-output",
        ],
    );
    excluded.extend(paths(&state, &["locks", "model.json"]));
    ProviderSpec {
        provider,
        roots: vec![share, state],
        items,
        excluded,
    }
}

fn paths(root: &std::path::Path, relative: &[&str]) -> Vec<PathBuf> {
    relative.iter().map(|entry| root.join(entry)).collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use chrono::DateTime;
    use uuid::Uuid;

    use super::*;
    use crate::config::{
        ArchiveConfig, EncryptionConfig, EncryptionMode, SourceOverrides, TargetConfig, VaultConfig,
    };

    #[test]
    fn overrides_all_provider_roots() {
        let mut config = sample_config();
        config.sources = SourceOverrides {
            claude_home: Some(PathBuf::from("/sources/claude")),
            codex_home: Some(PathBuf::from("/sources/codex")),
            grok_home: Some(PathBuf::from("/sources/grok")),
            kimi_home: Some(PathBuf::from("/sources/kimi")),
            opencode_share: Some(PathBuf::from("/sources/opencode/share")),
            opencode_state: Some(PathBuf::from("/sources/opencode/state")),
        };

        let specs = specifications(&config).unwrap();
        assert_eq!(specs.len(), Provider::ALL.len());
        assert_eq!(specs[0].roots, vec![PathBuf::from("/sources/claude")]);
        assert_eq!(
            specs[4].roots,
            vec![
                PathBuf::from("/sources/opencode/share"),
                PathBuf::from("/sources/opencode/state")
            ]
        );
    }

    #[test]
    fn never_includes_known_credentials() {
        let mut config = sample_config();
        config.sources.codex_home = Some(PathBuf::from("/sources/codex"));
        let specs = specifications(&config).unwrap();
        let codex = specs
            .iter()
            .find(|spec| spec.provider == Provider::Codex)
            .unwrap();

        assert!(
            codex
                .excluded
                .contains(&Path::new("/sources/codex/auth.json").to_path_buf())
        );
        assert!(
            codex
                .items
                .iter()
                .all(|entry| !entry.source_path.ends_with("auth.json"))
        );
    }

    fn sample_config() -> Config {
        Config {
            format_version: 1,
            vault: VaultConfig {
                id: Uuid::nil(),
                created_at: DateTime::from_timestamp(0, 0).unwrap(),
                state_path: PathBuf::from("/state"),
            },
            target: TargetConfig::Filesystem {
                path: PathBuf::from("/vault"),
            },
            archive: ArchiveConfig {
                chunk_size: 1024,
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
