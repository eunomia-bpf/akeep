# Provider compatibility

Akeep adapter version 1 preserves provider-native files; it does not normalize
them into a shared conversation schema. Exact recovery recreates the paths
listed below under a provider directory in the scratch target.

Real multi-provider discovery is exercised on the primary development machine.
CI uses synthetic fixtures and never contains private sessions.

| Provider | Recovery root | Included durable state | Consistency handling |
| --- | --- | --- | --- |
| AgentSight | `agentsight/` | Weekly `monitor/monitor-YYYY-Www.db` SQLite databases; `activity-summary.json` rollup | Live DBs are snapshotted with the SQLite online backup API (WAL/SHM never archived as separate objects). Invalid or busy DBs are skipped with a warning rather than failing the commit |
| Claude Code | `claude-code/` | `projects`, `sessions`, `file-history`, `plans`, `commands`, `tasks`, `todos`, `shell-snapshots`, `session-env`, `backups`, `history.jsonl` | Rotating shell/task state is staged early; symlinks are not followed |
| Codex CLI | `codex/` | `sessions`, `attachments`, `memories`, `shell_snapshots`, `automations`, `history.jsonl`, `session_index.jsonl`, state/log/goals/memories databases | Rotating shell/automation state is staged early; SQLite uses the backup API plus `integrity_check` |
| Grok CLI | `grok/` | `sessions`, `active_sessions.json`, `worktrees.db` | SQLite backup API for `worktrees.db` |
| Kimi Code | `kimi-code/` | `sessions`, `user-history`, `session_index.jsonl`, `workspaces.json` | Immutable/append-only files are read without following symlinks |
| OpenCode | `opencode/` | `opencode.db`, `storage`, `prompt-history.jsonl` | SQLite backup API for `opencode.db` |

### AgentSight details

AgentSight's long-lived `agentsight monitor` service writes weekly-rotated SQLite
databases under `~/.agentsight/monitor/` (override with `AGENTSIGHT_HOME` or
`[sources].agentsight_home`). Akeep includes only `monitor-*.db` files.

**Consistency:** each database is archived via SQLite's online backup API into a
private staging file. The archive therefore contains a consistent snapshot, not
a byte copy of a live WAL database. Sidecars (`-wal`, `-shm`, `-journal`), the
PID file, and lock files are never archived as separate objects.

**Activity summary (schema version 2):** every commit that includes at least one
successful AgentSight snapshot also writes `agentsight/activity-summary.json`.
The summary is a compact analysis rollup for dashboards; it is **not** an event
dump. Default encryption is off, so this file is readable by whoever holds the
archive. Schema version 2 keeps every v1 field and adds a time series,
per-session rows, and a program histogram.

#### Top-level fields

| Field | Meaning |
| --- | --- |
| `schema_version` | Document version (`2`). |
| `provider` | Fixed string `"agentsight"`. |
| `generated_at` | RFC3339 timestamp when the summary was built. |
| `databases[]` | One entry per successfully snapshotted weekly DB. |
| `totals` | Cross-DB aggregates of the v1 counters only. |

#### Per-database fields (v1, unchanged)

| Field | Meaning |
| --- | --- |
| `logical_path` | Archive path `agentsight/monitor/monitor-YYYY-Www.db` (week label only). |
| `week` | ISO week string parsed from the filename. |
| `schema_recognized` | `true` only when at least one **core** part (`tracked_sessions` or `monitor_windows`) had every column required to read it. Absent tables or missing columns yield `false` with zeroed counters — not “recognized but empty”. |
| `schema_parts` | Booleans for `tracked_sessions`, `monitor_windows`, `process_samples`, `file_samples`, `network_samples`. Lets a consumer tell “no activity” apart from “could not read this part”. |
| `time_range` | `first_seen_ms` / `last_seen_ms` over sessions. |
| `sessions.total` / `by_agent_type` / `by_status` | Counts; agent/status keys filtered to short safe tokens. |
| `windows.*` | Window count plus summed `cpu_ms`, peak `rss_bytes`, read/write bytes, file/network **target event counts**. |
| `samples.*` | Row counts only for process/file/network sample tables. |

#### Per-database fields (v2 additions)

| Field | Meaning |
| --- | --- |
| `activity_series` | Downsampled wall-clock series over `monitor_windows`. Absent if windows are not recognized or empty. |
| `activity_series.bucket_width_ms` | Chosen bucket width so the series stays near ~200 buckets (nice steps from 1 min up to whole days). Stated so a consumer can label the axis. |
| `activity_series.bucket_count` / `time_range` | Bucket count and aligned `[start_ms, end_ms)`. |
| `activity_series.buckets[]` | Per bucket: `start_ms`, concurrent `sessions`, `by_agent_type`, summed `cpu_ms` / `read_bytes` / `write_bytes` / `file_targets` / `network_targets`. Metrics land in the window-start bucket; concurrent session count uses window overlap. |
| `session_rows` | Capped per-session analysis rows. Absent if sessions are not recognized. |
| `session_rows.total` / `included` | Total sessions in the DB vs rows after the cap. |
| `session_rows.selection` | Always `"longest"`: keep the 100 longest sessions by duration (tie-break: most recent). |
| `session_rows.cpu_shape_points` | **Maximum** shape length used when downsampling (`16`). Each row’s `cpu_shape` array length is the actual point count (≤ this value); consumers must not assume a fixed length. |
| `session_rows.rows[]` | `id` (opaque 8-hex blake3 of session identity — never path-derived), `agent_type`, `status`, `first_seen_ms`, `last_seen_ms`, `duration_ms`, window aggregates (`total_cpu_ms`, `peak_rss_bytes`, read/write bytes, file/network target counts, `max_process_count`), and `cpu_shape` (values in `[0,1]` normalized by that session’s peak). |
| `session_rows.rows[].cpu_shape` | Gap-free profile: one point per window when there are fewer windows than the target; otherwise equal-count downsampling into the target length. **Never zero-pads** unmapped slots. A genuine zero-CPU window still appears as `0.0`. Array length is self-describing. |
| `programs` | Ranked program histogram from `process_samples.comm`. Absent if process samples are not recognized. |
| `programs.top_n` | Number of named programs kept before folding (`20`). |
| `programs.entries[]` | `comm` (basename only), `cpu_share` (fraction of summed sample `cpu_ms`), `sample_count`. Tail folded into a single `other` entry. |

#### Deliberately excluded from the summary

File paths, command lines, network hostnames/addresses, prompt/completion text,
environment variables, `session_path`, `cwd`, `display_id`, match evidence,
PIDs, `process_samples.command` / `cwd`, and file/network sample `target`
strings. Program names are basenames only (`node`, `cargo`); full paths and
rare identifying binaries in the long tail are either rejected or folded into
`other`. Schema introspection is defensive (`sqlite_master` /
`PRAGMA table_info`); a missing part omits that section and never fails the
commit.

The archived SQLite snapshot itself is a full consistent copy of the monitor
database and **does** contain paths, commands, and sample targets — that is the
restore artifact. The summary is the surface meant for unencrypted dashboard
rendering.

Empty or absent optional paths are not errors. A commit refuses to publish an
empty version, an unreadable included path, a special file, a changed
archive invariant, or a failed SQLite snapshot.

## Discovery overrides

Explicit `[sources]` paths in `config.toml` have highest precedence. Environment
variables are useful for isolated tests:

| Provider | Native/preferred environment variable | Akeep compatibility aliases |
| --- | --- | --- |
| AgentSight | `AGENTSIGHT_HOME` | — |
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

Run `akeep status --json` to see the exact resolved include inventory, declared
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
