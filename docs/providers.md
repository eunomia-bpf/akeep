# Provider compatibility

Akeep adapter version 1 preserves provider-native files; it does not normalize
them into a shared conversation schema. Exact recovery recreates the paths
listed below under a provider directory in the scratch target.

Real multi-provider discovery is exercised on the dogfood machine. CI uses
synthetic fixtures and never contains private sessions.

| Provider | Recovery root | Included durable state | Consistency handling |
| --- | --- | --- | --- |
| Claude Code | `claude-code/` | `projects`, `sessions`, `file-history`, `plans`, `commands`, `tasks`, `todos`, `shell-snapshots`, `session-env`, `backups`, `history.jsonl` | Rotating shell/task state is staged early; symlinks are not followed |
| Codex CLI | `codex/` | `sessions`, `attachments`, `memories`, `shell_snapshots`, `automations`, `history.jsonl`, `session_index.jsonl`, state/log/goals/memories databases | Rotating shell/automation state is staged early; SQLite uses the backup API plus `integrity_check` |
| Grok CLI | `grok/` | `sessions`, `active_sessions.json`, `worktrees.db` | SQLite backup API for `worktrees.db` |
| Kimi Code | `kimi-code/` | `sessions`, `user-history`, `session_index.jsonl`, `workspaces.json` | Immutable/append-only files are read without following symlinks |
| OpenCode | `opencode/` | `opencode.db`, `storage`, `prompt-history.jsonl` | SQLite backup API for `opencode.db` |

Empty or absent optional paths are not errors. A backup refuses to publish an
empty recovery point, an unreadable included path, a special file, a changed
archive invariant, or a failed SQLite snapshot.

## Discovery overrides

Explicit `[sources]` paths in `config.toml` have highest precedence. Environment
variables are useful for isolated tests:

| Provider | Native/preferred environment variable | Akeep compatibility aliases |
| --- | --- | --- |
| Claude Code | `CLAUDE_CONFIG_DIR` | `CLAUDE_HOME` |
| Codex CLI | `CODEX_HOME` | — |
| Grok CLI | `GROK_HOME` | — |
| Kimi Code | `KIMI_CODE_HOME` | — |
| OpenCode | `OPENCODE_SHARE_DIR`, `OPENCODE_STATE_DIR` | — |

Without an override, adapters use the provider's conventional locations under
the current user's home/XDG directories.

## Exclusion policy

Akeep includes a narrow allowlist instead of copying an entire provider home.
Known credentials and non-durable state are excluded, including:

- auth, OAuth, device credential, and research-key files;
- caches, telemetry, usage data, logs, downloads, and temporary paths;
- installed plugins, bundled binaries, vendor trees, and worktrees;
- IDE/control sockets, locks, daemon state, and transient tool output.

Run `akeep doctor --json` to see the exact resolved include inventory, declared
exclusions, unreadable paths, and skipped symlink count for the installed
version. Agent transcripts can themselves contain secrets printed during a
session; exclusion rules cannot redact source content.

## Recovery recognition

After an exact scratch recovery:

- point `CLAUDE_CONFIG_DIR` at `claude-code/`, run the native resume picker from
  the original project directory, and exit without starting a turn;
- point `CODEX_HOME` at `codex/`, run `codex resume --all`, and exit without
  starting a turn;
- validate every recovered SQLite file with `PRAGMA integrity_check`.

These checks operate only on the scratch copy. Akeep does not write
undocumented provider indexes into a live home or claim lossless cross-provider
session conversion.

Provider formats are not stable public APIs. A format change needs a new
adapter version, fixtures from the affected provider version, a recovery test,
and an update to this matrix before compatibility is claimed.
