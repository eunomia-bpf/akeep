# Akeep MVP specification

Status: v0.1 implementation complete; dogfood replacement gate in progress

Target: a dogfoodable Linux CLI

Primary success event: an integrity-checked checkout, not a successful upload

## 1. Product contract

Akeep gives coding-agent work an independent, Git-like version history without
depending on provider retention policies or undocumented cloud APIs.

The MVP is complete when this loop works on the dogfood machine:

```text
auto-discover -> commit -> chunk -> compress -> [encrypt] -> store
    -> fsck -> checkout to scratch -> compare -> provider smoke test
```

A commit that has not survived this loop is not counted as successful.

## 2. Non-negotiable invariants

1. Provider directories are read-only during discovery and commit.
2. Raw provider bytes are the source of truth. Parsed indexes are disposable.
3. No local deletion is propagated into historical commits.
4. Encryption is a vault-level user choice. Unencrypted vaults are fully
   supported and never silently migrated.
5. Recovery never overwrites live state without an explicit apply flag.
6. A commit is not marked complete until its manifest and all required objects
   are durable and structurally checked.
7. Interrupted commit, clone, and checkout operations are resumable or safely
   restartable.
8. Credentials, auth tokens, caches, and transient worktrees are excluded by
   provider-specific policy and shown by `akeep status`.
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
- Deduplication across commits and providers.
- Optional client-side authenticated encryption for payload objects.
- Stable logical paths separated from machine-specific absolute paths.
- Host, provider, adapter version, source metadata, and creation time recorded
  without storing secrets in plaintext metadata.

The initial implementation should prefer mature primitives and formats over
novel cryptography. Key recovery is part of the restore contract, not an
afterthought.

#### Commands and minimum UX

There is no required `add`. Akeep's provider adapters automatically discover
the narrow durable-state allowlist, so normal use adds no workflow to the agent
itself. The smallest useful loop is:

```console
akeep init
akeep status
akeep commit -m "before changing the toolchain"
```

`akeep status`

- Detect supported providers and configured storage targets.
- Report included/excluded paths, bytes, file counts, and unreadable paths.
- Check encryption key availability when enabled, credentials, bucket access,
  object versioning where relevant, and scheduler state.
- Support human-readable and `--json` output.

`akeep commit [-m <message>]`

- Acquire a per-vault lock.
- Snapshot live SQLite files through the SQLite backup API.
- Incrementally archive changed content.
- Upload only objects missing from the target.
- Publish the commit manifest last, with an optional message and parent ID.
- Never invoke remote delete.
- Return a non-zero status on any incomplete provider or target.

`akeep log`

- List completed commits by time, message, parent, host, providers, logical
  size, stored size, deduplication ratio, and integrity-check status.

`akeep diff [<from> [<to>]]`

- Default to `HEAD~1` and `HEAD`.
- Compare manifests without downloading file contents.
- Report added, modified, and removed paths plus per-provider byte changes.
- Support summary, `--name-only`, and JSON output.

`akeep fsck <commit>`

- Check manifest structure and required object presence.
- Optionally perform a full decrypt/decompress/hash pass.
- Detect a missing, truncated, reordered, or corrupted object.

`akeep checkout <commit> --to <directory>`

- Restore into a new mode-0700 scratch directory by default.
- Recreate provider-native logical layouts and consistent database files.
- Hash-check every restored file.
- Emit a machine-readable recovery report.
- Refuse a non-empty target unless an explicit conflict policy is provided.

`akeep clone <directory>`

- Copy the active filesystem or S3 repository into a new local bundle.
- Check every copied object's transport hash and the cloned commit chain.
- Include a directly usable config and independent mutable state.
- Never copy an age private identity; keep an incomplete marker on failure.

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
- Commit-retention policy with dry-run; no live-state offload yet.
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
- encryption mode cannot change per commit; migration is an explicit operation
  or creates a new vault;
- `status` always displays the active mode and whether storage operators can
  read the archive;
- when encryption is enabled, payload objects and sensitive manifest fields are
  encrypted locally and remote filenames are opaque object identifiers;
- encryption uses an authenticated, reviewed construction;
- an encrypted vault has an exportable offline recovery key;
- losing every device key without the recovery key is clearly documented as
  permanent data loss;
- `status` checks key access before encrypted commit, and checkout tests key
  access before downloading large payloads;
- plaintext staging files use mode 0700 directories and are removed after use.

No upload, account, or telemetry occurs merely because Akeep is installed. Exact
algorithms and key wrapping should be recorded in the archive-format
specification during implementation and covered by test vectors.

## 6. Replacement and dogfood gate

Akeep may replace the current weekly service only after all of these pass:

- [x] The five current providers are discovered with documented exclusions.
- [ ] At least three scheduled Akeep commits complete over at least 14 days.
- [x] The latest commit is fully checked out into scratch and byte hashes
      match the snapshotted inputs.
- [ ] A commit at least one week old is fully checked out.
- [x] A deliberately corrupted copied archive is rejected by `fsck`.
- [x] An interrupted commit resumes or safely restarts without publishing a
      false-complete commit.
- [x] A live SQLite database is backed up under concurrent writes and passes
      integrity checks after recovery.
- [x] Claude Code and Codex can each recognize a restored fixture in an isolated
      test home, or the exact incompatibility is documented.
- [x] The old S3 backup remains untouched throughout shadow mode.
- [x] Operator runbook documents how to re-enable the old timer.

Only then should the old timer be disabled. Its remote backup remains a
fallback until Akeep has at least one additional successful restore drill.

### Shadow-run evidence

On 2026-07-27 UTC, the first real five-provider shadow run published a recovery
commit containing 52,432 files and 55,206,535,333 logical bytes. A full isolated
recovery reproduced every file and byte, all seven recovered SQLite databases
passed `PRAGMA integrity_check`, and current Claude Code and Codex clients
recognized restored native state without a model request. The commit now has a
local full-integrity receipt.

Its content chunks occupy 10,690,998,971 stored bytes: 51.42 GiB logical became
9.96 GiB stored, a 5.16:1 ratio or 80.6% reduction. Within that first commit,
duplicate chunks account for 188,747,292 bytes (0.34%), so almost all first-run
savings came from zstd compression. Cross-commit deduplication is the larger
incremental win: an unchanged second commit uploads no new content objects.

This is evidence for one current commit, not a substitute for the
remaining 14-day and week-old recovery gates. The previous backup timer remains
enabled.

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
 └─ fsck, diff, clone, and checkout
```

Do not split provider adapters, archive format, sync protocol, or cloud
components into separate repositories until an external consumer requires it.

## 8. Metrics

The MVP dashboard should emphasize reliability:

- recovery drills attempted and passed;
- age of the last fully checked commit;
- protected vs discovered bytes/files;
- commit duration and incremental uploaded bytes;
- logical-to-stored compression ratio;
- deduplication ratio;
- integrity-check duration;
- provider compatibility failures;
- restore point objective achieved or missed.

Stars, indexed sessions, and uploaded gigabytes are not proof of recovery.
