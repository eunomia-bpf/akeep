# Changelog

All notable changes are documented here. Akeep follows Semantic Versioning once
the first release is tagged.

## [Unreleased]

## [0.1.0-alpha.1] - 2026-07-31

### Added

- Git-like agent-history commits with optional messages, parent links, `HEAD`
  and `HEAD~N` references, plus `log`, `diff`, and scratch `checkout`.
- Manifest-only file/provider diffs with human, name-only, and JSON output.
- Exact local/S3-to-local repository cloning with per-object transport checks,
  incomplete markers, and a directly usable bundled configuration.
- Five-provider discovery and credential-aware exclusions.
- Consistent SQLite snapshots for live agent databases.
- Incremental fixed-chunk archives with BLAKE3, zstd, and cross-snapshot
  deduplication.
- Local filesystem and S3-compatible targets.
- Optional age X25519 client-side encryption; plaintext remains supported.
- Versioned manifests, quick/full integrity receipts, safe scratch recovery,
  and machine-readable reports.
- Linux systemd user scheduling.
- Provider-scoped recovery drills for machines without enough scratch space for
  an entire commit.
- Rebuildable local Claude Code/Codex FTS search, exact JSON export, readable
  Markdown export, and reviewed semantic handoff bundles.
- Fault-injection, concurrent-WAL, remote interruption, and scale tests.
- Bounded cross-file archive concurrency with per-object coordination, keeping
  large multi-provider first backups practical without unbounded memory.
- Bounded parallel full integrity checks and scratch recovery, including ordered
  reconstruction and hash checks for multi-chunk files.
- Native `CLAUDE_CONFIG_DIR` discovery with a backwards-compatible
  `CLAUDE_HOME` alias.
- Early private staging for rotating shell/task state, preventing an active
  provider from deleting a discovered file during a long first backup.
- Four-worker bounded commit/fsck/checkout execution with RAM, CPU, and disk
  preflight reporting.
- Bounded S3 object staging and recursive batch uploads, replacing one AWS CLI
  process per new content object without changing the archive format.
- Native Linux x86_64/ARM64 and macOS Intel/Apple Silicon release archives,
  checksums, provenance attestations, and public-artifact install smoke tests.

### Changed

- The pre-alpha CLI now uses the clean command set `status`, `commit`, `log`,
  `fsck`, and `checkout`. The former command names are not retained as aliases.
  Re-run `akeep schedule install --weekly` after upgrading so an existing
  generated timer invokes `commit`.
- Product positioning now leads with privacy-first version history for agents;
  integrity checks and exact recovery remain core mechanisms.

[Unreleased]: https://github.com/eunomia-bpf/akeep/compare/v0.1.0-alpha.1...HEAD
[0.1.0-alpha.1]: https://github.com/eunomia-bpf/akeep/releases/tag/v0.1.0-alpha.1
