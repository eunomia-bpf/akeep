# Changelog

All notable changes are documented here. Akeep follows Semantic Versioning once
the first release is tagged.

## [Unreleased]

### Added

- Five-provider discovery and credential-aware exclusions.
- Consistent SQLite snapshots for live agent databases.
- Incremental fixed-chunk archives with BLAKE3, zstd, and cross-snapshot
  deduplication.
- Local filesystem and S3-compatible targets.
- Optional age X25519 client-side encryption; plaintext remains supported.
- Versioned manifests, quick/full verification receipts, safe scratch recovery,
  and machine-readable reports.
- Linux systemd user scheduling.
- Provider-scoped recovery drills for machines without enough scratch space for
  an entire recovery point.
- Rebuildable local Claude Code/Codex FTS search, exact JSON export, readable
  Markdown export, and reviewed semantic handoff bundles.
- Fault-injection, concurrent-WAL, remote interruption, and scale tests.
- Bounded cross-file archive concurrency with per-object coordination, keeping
  large multi-provider first backups practical without unbounded memory.

[Unreleased]: https://github.com/eunomia-bpf/akeep/commits/main
