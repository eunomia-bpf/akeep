# AgentSight provider report

## What was added

A sixth provider adapter, **AgentSight**, following the existing
`ProviderSpec { provider, roots, items, excluded }` pattern, plus a schema-v2
activity summary that keeps every v1 aggregate and adds analysis-shaped data.

| Piece | Detail |
| --- | --- |
| Provider id | `agentsight` |
| Display name | AgentSight |
| Config key | `[sources].agentsight_home` |
| Environment override | `AGENTSIGHT_HOME` |
| Default root | `~/.agentsight` |
| Included files | `monitor/monitor-YYYY-Www.db` (discovered dynamically) |
| Excluded (declared) | `monitor/monitor.pid`, `monitor/monitor.lock`, `monitor/monitor.db-journal` |
| Never archived as objects | `-wal`, `-shm`, `-journal` sidecars (not matched by discovery; online backup subsumes WAL) |
| Consistency | SQLite online backup API → staged snapshot → `ArchivedFileKind::SqliteSnapshot` |
| Soft failures | Invalid/busy AgentSight DBs are **skipped with a warning**; commit continues |
| Summary path | `agentsight/activity-summary.json` (stable logical path) |
| Summary schema | **version 2** (v1 fields preserved byte-compatible for older consumers) |

Code touchpoints:

- `src/providers.rs` — enum, discovery, exclusions, unit tests
- `src/config.rs` — `SourceOverrides.agentsight_home`
- `src/source.rs` — soft snapshot path, busy budget (15s for AgentSight), activity summary v2
- Docs: `README.md`, `docs/providers.md`, `docs/configuration.md`, `CHANGELOG.md`
- Fixtures/tests/demo updated for the sixth provider

Confirmed against the AgentSight repo (read-only): weekly path is
`~/.agentsight/monitor/monitor-YYYY-Www.db` via `monitor_db_path_for_home` /
`MONITOR_SCHEMA` in `collector/src/cmd_monitor.rs`. The agentsight tree was
not modified.

## Snapshot behavior

Live AgentSight monitor databases use WAL. Akeep does **not** byte-copy the
live `.db` file. Each matching file is opened read-only and copied with
`rusqlite::backup::Backup` into a private staging file; `PRAGMA integrity_check`
must return `ok` before archival. WAL/SHM are never listed as separate archive
objects.

- Non-AgentSight SQLite sources still fail the commit after a long busy wait
  (five minutes), unchanged.
- AgentSight sources wait up to **15 seconds** on `Busy`/`Locked`, then skip
  with `warning: skipping AgentSight SQLite snapshot ...` and leave the rest of
  the commit intact.
- Invalid non-SQLite content under `monitor-*.db` is likewise skipped for
  AgentSight only.
- An absent `~/.agentsight` (or empty monitor dir) contributes nothing, same as
  an absent `~/.codex`.

Unit coverage includes concurrent WAL writers and an open write transaction
held by another connection; the snapshot remains openable with the expected
tables.

## Activity summary — schema v2 field list

Logical path: **`agentsight/activity-summary.json`**.

Default encryption is `None`, so this rollup is readable by whoever holds the
archive. Only dashboard-safe fields are included. Schema version is **2**.
Every v1 field remains with the same meaning; older consumers that ignore
unknown keys continue to work.

### Top level

| Field | Why safe to display |
| --- | --- |
| `schema_version` | Constant document version (`2`), no user content. |
| `provider` | Fixed string `"agentsight"`. |
| `generated_at` | RFC3339 timestamp of summary creation on the backup machine. |
| `databases[]` | Per-DB rollups (see below). |
| `totals` | Cross-DB aggregates of the same safe counters as v1. |

### Per database — v1 fields (unchanged)

| Field | Why safe to display |
| --- | --- |
| `logical_path` | Archive-relative path of the form `agentsight/monitor/monitor-YYYY-Www.db` (week label only, not a host filesystem path). |
| `week` | ISO week string parsed from the filename (`2026-W25`). |
| `schema_recognized` | **Honest in v2:** `true` only when at least one core part (`tracked_sessions` or `monitor_windows`) had every column required to read it. Tables present with the wrong columns yield `false` and zeros — not “recognized but empty”. |
| `time_range.first_seen_ms` / `last_seen_ms` | Aggregate min/max session epoch ms. |
| `sessions.total` | Count of rows in `tracked_sessions`. |
| `sessions.by_agent_type` | Counts keyed by short agent tokens (`claude`, `codex`, …); labels filtered by `is_safe_label`. |
| `sessions.by_status` | Counts keyed by short status tokens (`active`, `ended`, …); same filter. |
| `windows.total` | Count of `monitor_windows` rows. |
| `windows.total_cpu_ms` | Sum of window CPU time. |
| `windows.peak_rss_bytes` | Max window RSS. |
| `windows.total_read_bytes` / `total_write_bytes` | Summed process I/O counters. |
| `windows.total_file_target_events` / `total_network_target_events` | Sum of **counts** of file/network targets, not paths or endpoints. |
| `samples.process_rows` / `file_rows` / `network_rows` | Row counts only. |

### Per database — recognition (v2)

| Field | Why safe to display |
| --- | --- |
| `schema_parts.tracked_sessions` | Whether session table + required columns were present. |
| `schema_parts.monitor_windows` | Whether windows table + required columns were present. |
| `schema_parts.process_samples` | Whether process samples + `comm`/`cpu_ms` were present. |
| `schema_parts.file_samples` / `network_samples` | Whether those tables exist (row counts only). |

A consumer can distinguish “no activity this week” (`schema_recognized: true`,
zero counters) from “could not read it” (`schema_recognized: false` or a
specific `schema_parts.*: false`).

### Per database — activity series (v2)

Absent when windows are not recognized or contain no rows.

| Field | Why safe to display |
| --- | --- |
| `activity_series.bucket_width_ms` | Chosen fixed wall-clock width (see bucketing choices). |
| `activity_series.bucket_count` | Number of buckets emitted. |
| `activity_series.time_range.start_ms` / `end_ms` | Aligned series bounds (half-open). |
| `activity_series.buckets[].start_ms` | Bucket start epoch ms. |
| `activity_series.buckets[].sessions` | Concurrent distinct sessions whose windows **overlap** the bucket. |
| `activity_series.buckets[].by_agent_type` | That concurrent set broken down by safe agent tokens. |
| `activity_series.buckets[].cpu_ms` / `read_bytes` / `write_bytes` / `file_targets` / `network_targets` | Metrics summed into the bucket of each window’s **start** (no double-count across buckets). |

### Per database — session rows (v2)

Absent when sessions are not recognized. Window aggregates are zero when
windows are missing.

| Field | Why safe to display |
| --- | --- |
| `session_rows.total` | Full session count in the DB (before cap). |
| `session_rows.included` | Rows after the cap. |
| `session_rows.selection` | Always `"longest"`. |
| `session_rows.cpu_shape_points` | **Maximum** shape length when downsampling (`16`). Not a fixed per-row length. |
| `session_rows.rows[].id` | Opaque 8-hex blake3 of `(session_id, root_pid, root_starttime_ticks)`. **Not** path-derived; `display_id` is never used. |
| `session_rows.rows[].agent_type` / `status` | Safe short tokens only. |
| `session_rows.rows[].first_seen_ms` / `last_seen_ms` / `duration_ms` | Timestamps and duration. |
| `session_rows.rows[].total_cpu_ms` / `peak_rss_bytes` / `total_read_bytes` / `total_write_bytes` / `total_file_targets` / `total_network_targets` / `max_process_count` | Per-session window aggregates (counts and resources only). |
| `session_rows.rows[].cpu_shape` | Gap-free floats in `[0,1]`, peak-normalized. Length = actual point count (self-describing): natural window count when fewer than the target, else downsampled. Never zero-pads unmapped slots; genuine zero-CPU windows still emit `0.0`. |

### Per database — programs (v2)

Absent when process samples are not recognized.

| Field | Why safe to display |
| --- | --- |
| `programs.top_n` | Cap before folding (`20`). |
| `programs.entries[].comm` | Process **basename** only (`node`, `cargo`, `git`), filtered by `is_safe_comm`. |
| `programs.entries[].cpu_share` | Fraction of summed sample `cpu_ms` (3 decimal places). |
| `programs.entries[].sample_count` | Number of sample rows for that `comm`. |
| `programs.entries[]` tail | Folded into a single `comm: "other"` entry. |

### Totals object (v1, unchanged)

| Field | Why safe to display |
| --- | --- |
| `totals.databases` | Number of successfully snapshotted weekly DBs. |
| `totals.sessions` / `windows` | Summed counts. |
| `totals.by_agent_type` | Merged agent-type histogram. |
| `totals.time_range` | Min/max across databases. |
| `totals.total_cpu_ms` / `peak_rss_bytes` / `total_read_bytes` / `total_write_bytes` | Resource aggregates only. |

Queries check `sqlite_master` and `PRAGMA table_info` before selecting. A
missing part omits that v2 section (or zeros the v1 counters) and never fails
the commit.

## Bucketing and capping choices (and why)

### Activity series

- **Target:** about **200 buckets** across the database’s window time range, so
  a multi-day or multi-week DB stays chartable instead of exploding.
- **Width selection:** `ceil(range / 200)`, floored at 1 minute, then snapped
  up to a nice step: 1m, 5m, 15m, 30m, 1h, 2h, 3h, 6h, 12h, 1d, then whole days.
  The chosen width is written to `bucket_width_ms` so axis labels are correct.
- **Hard cap:** if nice rounding still exceeds 400 buckets, width is recomputed
  as `ceil(range / 200)` snapped to whole minutes.
- **Metrics vs concurrency:** resource counters land in the bucket of
  `window_start_ms` (no double-count). Concurrent session count uses window
  overlap so a long window spans consecutive buckets.
- **Empty buckets** are kept so the axis is continuous wall-clock time; at
  ~200 buckets this stays small.

### Session rows

- **Cap:** **100** sessions.
- **Selection:** **`longest`** by `duration_ms` (tie-break: most recent
  `last_seen_ms`). Longest is preferred over most-recent because the CPU shape
  exists to surface stuck retry loops and other long-running anomalies; totals
  already cover volume.
- **Presentation order** after the cap: most recent `last_seen_ms` first.
- **`total` is always reported** next to `included` so a consumer knows when
  the list is a subset.
- **CPU shape (gap-free):** target max **16** points. Windows are sorted by
  `window_start_ms`. If fewer windows than the target, emit **one point per
  window** (no zero padding / upsampling). If at least the target count, use
  equal-count contiguous downsampling so every bin gets ≥1 window. Peak-
  normalize within the session (not globally), round to 3 decimals. Genuine
  zero-CPU windows remain `0.0`. Array length is the actual point count.

### Program histogram

- **Top 20** by summed sample `cpu_ms`, then sample count.
- **Tail → `other`:** prevents long-tail bloat and reduces the chance of a rare,
  identifying binary name dominating the document.
- **`comm` only:** basenames that pass `is_safe_comm` (same token rules as
  labels, plus no `/` or `\`). The reserved name `other` is not accepted as an
  input `comm` so it cannot collide with the folded entry.

### Size budget

Design target: **low hundreds of kilobytes** even for a heavy week.

Rough upper bound with caps:

- ~200 series buckets × ~150 B ≈ 30 KiB
- 100 session rows × (~200 B + 16 floats) ≈ 30 KiB
- 21 program entries ≈ 1 KiB
- v1 aggregates ≈ a few KiB

Measured fixture (below): **38 921 bytes**.

## Deliberately left out

Anything that can encode host content or free text was excluded from the
summary even when present in the monitor schema:

| Source column / data | Reason |
| --- | --- |
| `tracked_sessions.session_path`, `cwd` | Filesystem paths. |
| `tracked_sessions.command` | Full command lines. |
| `tracked_sessions.display_id` | Often path-derived (e.g. `claude:/home/...`). Replaced by opaque `id` when a UI handle is needed. |
| `tracked_sessions.match_evidence`, `match_confidence` | Evidence strings / scores not needed for a dashboard rollup. |
| `tracked_sessions.session_id`, PIDs, starttime ticks | Process identity on the host; hashed into opaque `id` only, never emitted raw. |
| `process_samples.command`, `cwd`, PIDs | Command lines and paths. |
| `process_samples.comm` beyond top-N | Long-tail basenames folded into `other`. |
| `file_samples.target` | File paths. |
| `network_samples.target` | Host:port / network endpoints. Counts only via window aggregates. |
| Prompt/completion text, env vars | Not in the monitor schema; would never be added to this rollup. |
| Full event dumps | Summary is analysis-shaped but still aggregate/downsampled. |

**Left out of this change (possible later, not needed for dashboards):**

- Cross-database merged series (each weekly DB has its own series; totals stay
  v1-style aggregates).
- Omitting empty series buckets (kept for a continuous wall-clock axis).
- Network/file target **names** (explicitly out of scope; counts only).
- Any field derived from `display_id` or session path.

**Note:** the archived SQLite snapshot itself is a full consistent copy of the
monitor database and **does** contain paths, commands, and sample targets. That
is intentional exact recovery of AgentSight durable state. The summary is the
surface meant for unencrypted dashboard rendering; the DB is the restore
artifact.

## Verification

### Automated

- `cargo build` — pass
- `cargo test` — pass (AgentSight discovery, soft-skip invalid DB, snapshot
  under held write transaction, concurrent WAL, privacy summary test, honest
  schema recognition when columns are missing, bucket-width helper)
- `cargo clippy --all-targets -- -D warnings` — pass
- `cargo fmt --all` — applied

### End-to-end (temporary vault, schema v2)

Realistic monitor DB built from the **true** `MONITOR_SCHEMA` in
`agentsight/collector/src/cmd_monitor.rs` (read-only; agentsight repo not
modified):

- 5 sessions across agent types `claude`, `codex`, `grok`, `kimi`
- Multi-day range (Mon–Fri of ISO week 2026-W25)
- 151 windows, 361 process samples, file/network samples with real path and
  hostname payloads in the DB
- One long Claude session whose CPU is high for ~3.3 h then flatlines (stuck
  retry loop)
- Sidecars present: `monitor.pid`, `-wal`, `monitor.lock` (must not archive)

Provider pointed only at that tree; other providers overridden to missing paths.

```text
=== akeep status ===
Providers:
  agentsight present          1 files   108.0 KiB
  …others not found…
Result: ready

=== akeep commit ===
Committed agent history 20260806T033523.644Z-69ee4403
Files: 2
Logical bytes: 149513
Unique objects: 2
New objects: 2 (13378 stored bytes)

=== akeep checkout ===
Checked out 20260806T033523.644Z-69ee4403
Files: 2
…/recovery/agentsight/activity-summary.json
…/recovery/agentsight/monitor/monitor-2026-W25.db
```

**Measured summary size: 38 921 bytes.**

Key recovered summary properties:

| Property | Value |
| --- | --- |
| `schema_version` | `2` |
| `schema_recognized` / all `schema_parts` | `true` |
| `activity_series.bucket_width_ms` | `3600000` (1 hour) |
| `activity_series.bucket_count` | `105` |
| `session_rows.total` / `included` | `5` / `5` |
| `session_rows.selection` | `longest` |
| Stuck Claude `cpu_shape` | `[0.9, 0.76, 1.0, 0.7, 0.96, 0.78, 0.92, 0.79, 0.746, 0.024, 0.03, 0.024, 0.03, 0.024, 0.03, 0.024]` |
| `programs.entries` (comms) | `grok`, `claude`, `node`, `python`, `kimi`, `codex`, `cargo`, `rare-internal-tool-xyz`, `git` |

Privacy grep of the produced JSON (no matches):

- filesystem path prefixes (`/home/`, `/data/`, `/srv/`, `/tmp/`, `/opt/`)
- hostnames (`api.anthropic.com`, `api.openai.com`, `api.x.ai`)
- command lines (`claude --dangerously-skip-permissions`, `codex resume`, …)
- `display_id`, `session_path`, `match_evidence`, `.env`, `dataset.parquet`

Basenames such as `rare-internal-tool-xyz` may appear in the program histogram
(they are not paths). Full paths and argv never appear. Sidecars were absent
from checkout.

## cpu_shape fix (post-v2)

### Defect

The first v2 implementation always allocated `cpu_shape_points` (16) slots and
mapped each window into a time-proportional index. Sessions with fewer windows
than the target left unmapped slots at `0.0` — artificial gaps, not real
zero-CPU samples. A 12-window session became a 16-point comb, which hides the
flat-tail stuck-retry signature sparklines are meant to show.

Time-based binning also biased the peak toward the last bin when the session
end landed on the final slot even if mid-session samples carried more CPU.

### Fix

`build_cpu_shape` now:

1. Sorts windows by start time.
2. **n ≤ target:** emit n points (one per window). No padding.
3. **n > target:** equal-count partition into `target` bins (every bin ≥1
   window); sum CPU per bin.
4. Peak-normalize; preserve genuine `0.0` from zero-CPU windows.
5. Document `session_rows.cpu_shape_points` as the **max** target; row array
   length is the actual count.

### Normalization check

With equal-count downsampling, `1.0` lands on the bin that holds the true peak
sample(s), not automatically the last bin. A mid-session peak fixture confirms
the last point is not forced to 1.0. Rows that end at 1.0 do so only when the
last window/bin actually carries the session max (real data).

### Regression tests

- `cpu_shape_short_session_is_natural_length_without_padding_zeros` — 12
  non-zero windows → 12 points, no zeros, monotonic.
- `cpu_shape_preserves_genuine_zero_cpu_window` — real zeros kept.
- `cpu_shape_flat_tail_survives_downsampling` — high then flat-low tail remains
  flat and low after 16-point downsample.
- `cpu_shape_peak_is_true_max_not_last_bin_bias` — mid peak stays mid.

### Not done (per request)

- No commit or push; working tree left for review.
- Agentsight repo untouched.
- Archive format, encryption behavior, and storage backends unchanged.
