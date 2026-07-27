# Akeep MVP specification

Status: v0.1 implementation complete; dogfood replacement gate in progress

Target: a dogfoodable Linux CLI

Primary success event: a verified recovery, not a successful upload

## 1. Product contract

Akeep preserves coding-agent work independently of provider retention policies
and undocumented local formats.

The MVP is complete when this loop works on the dogfood machine:

```text
discover -> snapshot -> chunk -> compress -> [encrypt] -> store
    -> verify -> recover to scratch -> compare -> provider smoke test
```

A backup that has not survived this loop is not counted as successful.

## 2. Non-negotiable invariants

1. Provider directories are read-only during discovery and backup.
2. Raw provider bytes are the source of truth. Parsed indexes are disposable.
3. No local deletion is propagated into historical recovery points.
4. Encryption is a vault-level user choice. Unencrypted vaults are fully
   supported and never silently migrated.
5. Recovery never overwrites live state without an explicit apply flag.
6. A recovery point is not marked complete until its manifest and all required
   objects are durable and verifiable.
7. Interrupted backup and recovery operations are resumable or safely
   restartable.
8. Credentials, auth tokens, caches, and transient worktrees are excluded by
   provider-specific policy and shown by `akeep doctor`.
9. No telemetry or network access occurs unless a remote storage target is
   explicitly configured.

## 3. Scope

### P0: required for v0.1

#### Source adapters

Raw backup parity with the existing dogfood service:

| Provider | Required state |
| --- | --- |
| Claude Code | projects, file history, plans, commands, provider backups |
| Codex CLI | sessions, history/index files, attachments, memories, shell snapshots, consistent SQLite snapshots |
| Grok CLI | sessions |
| Kimi Code | sessions, session index, user history |
| OpenCode | consistent SQLite snapshot, storage, prompt history |

An adapter owns only discovery, exclusion policy, stable logical paths, and
consistent snapshot rules. It does not parse conversations in v0.1.

#### Storage targets

- Local filesystem target. Its root may be on an internal disk, external disk,
  NAS mount, or directory synchronized by another tool.
- S3-compatible target, including AWS S3 and compatible services.

The archive layer must not depend on S3 semantics. Storage targets implement a
small object contract: put-if-absent, get, stat, list by prefix, and atomic
publication of a completed manifest.

#### Archive

- Versioned, self-describing manifest.
- Content-defined or fixed-size chunking selected by benchmark, not intuition.
- Per-chunk strong content hash.
- Compression suitable for JSONL, JSON, SQLite snapshots, and text artifacts.
- Deduplication across recovery points and providers.
- Optional client-side authenticated encryption for payload objects.
- Stable logical paths separated from machine-specific absolute paths.
- Host, provider, adapter version, source metadata, and creation time recorded
  without storing secrets in plaintext metadata.

The initial implementation should prefer mature primitives and formats over
novel cryptography. Key recovery is part of the restore contract, not an
afterthought.

#### Commands

`akeep doctor`

- Detect supported providers and configured storage targets.
- Report included/excluded paths, bytes, file counts, and unreadable paths.
- Check encryption key availability when enabled, credentials, bucket access,
  object versioning where relevant, and scheduler state.
- Support human-readable and `--json` output.

`akeep backup`

- Acquire a per-vault lock.
- Snapshot live SQLite files through the SQLite backup API.
- Incrementally archive changed content.
- Upload only objects missing from the target.
- Publish the recovery-point manifest last.
- Never invoke remote delete.
- Return a non-zero status on any incomplete provider or target.

`akeep snapshots`

- List completed recovery points by time, host, providers, logical size,
  stored size, deduplication ratio, and verification status.

`akeep verify <recovery-point>`

- Verify manifest authenticity and required object presence.
- Optionally perform a full decrypt/decompress/hash pass.
- Detect a missing, truncated, reordered, or corrupted object.

`akeep recover <recovery-point> --to <directory>`

- Recover into a new mode-0700 scratch directory by default.
- Recreate provider-native logical layouts and consistent database files.
- Verify every recovered file.
- Emit a machine-readable recovery report.
- Refuse a non-empty target unless an explicit conflict policy is provided.

`akeep schedule install --weekly`

- Install a systemd user service/timer on Linux.
- Use a persistent timer, randomized delay, low I/O priority, and the same
  per-vault lock as manual runs.
- Make uninstall leave archives and configuration untouched.

### P1: immediately after the replacement gate

- [Implemented early] Claude Code and Codex local full-text search with a
  rebuildable SQLite index.
- [Implemented early] Markdown and JSON export.
- [Implemented early] Semantic handoff bundle between Claude Code and Codex
  containing goal,
  decisions, changed files, commands/results, test status, repository state,
  artifacts, and open tasks.
- Recovery-point retention policy with dry-run; no live-state offload yet.
- macOS scheduler integration.

### Explicitly out of scope for v0.1

- Desktop or web UI.
- Managed cloud service, accounts, billing, or team features.
- Browser-side search or server-side plaintext/embedding indexes.
- Native cross-agent session injection.
- Automatic deletion or movement of live provider history.
- Support for every IDE and agent.
- Token/cost dashboards.
- Workspace filesystem backup beyond metadata and explicitly attached artifacts.

## 4. Recovery semantics

Akeep distinguishes three outputs:

- **Exact recovery:** reconstruct provider-native files from archived bytes.
- **Semantic handoff:** create a portable description another agent can use.
- **Native cross-provider import:** write undocumented target-provider state.

Only exact recovery is part of v0.1. Semantic handoff is P1. Native
cross-provider import is experimental at most and must never be advertised as
lossless.

Recovery modes:

1. `--to <scratch>` is always available and cannot touch live provider state.
2. `--apply` is added only after per-provider collision, index, process-lock,
   backup, and rollback rules are implemented.
3. Older provider versions remain recoverable as files even when current
   software can no longer index them.

## 5. Privacy and optional encryption

Privacy-first means local ownership and explicit network behavior, not mandatory
encryption. Akeep supports both unencrypted and client-encrypted vaults.

For v0.1:

- `encryption = "none"` is a supported, tested mode, not a hidden debug path;
- client-side authenticated encryption is an optional vault-level mode;
- local targets may default to `none`;
- remote targets recommend encryption but allow the user to continue without
  it after a clear disclosure;
- encryption mode cannot change per backup; migration is an explicit operation
  or creates a new vault;
- `doctor` always displays the active mode and whether storage operators can
  read the archive;
- when encryption is enabled, payload objects and sensitive manifest fields are
  encrypted locally and remote filenames are opaque object identifiers;
- encryption uses an authenticated, reviewed construction;
- an encrypted vault has an exportable offline recovery key;
- losing every device key without the recovery key is clearly documented as
  permanent data loss;
- `doctor` checks key access before encrypted backup, and recovery tests key
  access before downloading large payloads;
- plaintext staging files use mode 0700 directories and are removed after use.

No upload, account, or telemetry occurs merely because Akeep is installed. Exact
algorithms and key wrapping should be recorded in the archive-format
specification during implementation and covered by test vectors.

## 6. Replacement and dogfood gate

Akeep may replace the current weekly service only after all of these pass:

- [x] The five current providers are discovered with documented exclusions.
- [ ] At least three scheduled Akeep backups complete over at least 14 days.
- [ ] The latest recovery point is fully recovered into scratch and byte hashes
      match the snapshotted inputs.
- [ ] A recovery point at least one week old is fully recovered.
- [ ] A deliberately corrupted copied archive is rejected by `verify`.
- [x] An interrupted backup resumes or safely restarts without publishing a
      false-complete recovery point.
- [x] A live SQLite database is backed up under concurrent writes and passes
      integrity checks after recovery.
- [ ] Claude Code and Codex can each recognize a restored fixture in an isolated
      test home, or the exact incompatibility is documented.
- [x] The old S3 backup remains untouched throughout shadow mode.
- [x] Operator runbook documents how to re-enable the old timer.

Only then should the old timer be disabled. Its remote backup remains a
fallback until Akeep has at least one additional successful restore drill.

## 7. Implementation shape

Start with one repository and one binary. Rust is the current recommendation
for a portable single executable, safe filesystem handling, streaming
compression/encryption, and predictable resource usage.

Keep boundaries simple:

```text
CLI
 ├─ provider discovery and snapshots
 ├─ archive pipeline
 │   ├─ chunk
 │   ├─ compress
 │   ├─ encrypt
 │   └─ manifest
 ├─ storage target
 │   ├─ filesystem
 │   └─ S3-compatible
 └─ verify and recover
```

Do not split provider adapters, archive format, sync protocol, or cloud
components into separate repositories until an external consumer requires it.

## 8. Metrics

The MVP dashboard should emphasize reliability:

- recovery drills attempted and passed;
- last verified recovery-point age;
- protected vs discovered bytes/files;
- backup duration and incremental uploaded bytes;
- logical-to-stored compression ratio;
- deduplication ratio;
- verification duration;
- provider compatibility failures;
- restore point objective achieved or missed.

Stars, indexed sessions, and uploaded gigabytes are not proof of recovery.
