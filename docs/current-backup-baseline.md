# Current dogfood backup baseline

Observed on 2026-07-26. This document defines the minimum operational baseline
Akeep must meet before it can replace the service already running on the
dogfood machine.

No credentials, bucket names, or transcript contents are recorded here.

## Existing service

The current `claude-codex-sync-aws` installation is a weekly systemd user
service. Its latest observed run completed successfully, its remote bucket had
versioning enabled, and seven dated manifests were present.

The installed service is ahead of its public repository and supports five
providers:

- Claude Code
- Codex CLI
- Grok CLI
- Kimi Code
- OpenCode

It performs file-level incremental upload to AWS S3 without remote deletion.
Codex and OpenCode SQLite files are copied with the SQLite backup API before
upload. S3 server-side AES-256 encryption and bucket versioning are enabled, but
there is no client-side encryption.

## Capability comparison

| Capability | Existing service | Akeep v0.1 requirement |
| --- | --- | --- |
| Scheduled backup | Weekly persistent systemd timer | Equivalent or better |
| Concurrency control | Process lock | Per-vault lock |
| Providers | Claude, Codex, Grok, Kimi, OpenCode | Raw backup parity for all five |
| Live SQLite | Consistent snapshots | Consistent snapshots plus integrity test |
| Incremental transfer | AWS file-level sync | Chunk/object-level incremental |
| Remote target | AWS S3 only | Local filesystem and S3-compatible |
| Compression | None | Streaming compression |
| Client-side encryption | None | Required for remote objects |
| Remote deletion | Never | Never in v0.1 |
| History | S3 object versions | Immutable recovery points |
| Manifest | Presence booleans and timestamp | Versioned file/object hashes and adapter metadata |
| Verification | Bucket reachability | Full manifest/object/content verification |
| Recovery | Download current prefix to scratch | Recover any point, verify every file, report conflicts |
| Provider restore test | Manual/not encoded | Automated isolated smoke test for Claude and Codex |
| Search/handoff | None | P1 after replacement gate |

## Existing strengths Akeep must preserve

- The current service is simple and has few moving parts.
- It never mirrors local deletion to the remote bucket.
- It snapshots live databases instead of copying inconsistent SQLite files.
- It can run unattended after a missed timer event.
- Its remote object layout is transparent and can be recovered with standard
  AWS tooling.

Akeep must not trade these properties for a sophisticated archive that is
impossible to debug. The archive format needs a documented recovery path and
standalone verification fixtures.

## Existing gaps Akeep is meant to close

- A bucket reader can read raw transcripts and private code.
- Large changing files upload again in full.
- There is no compression or cross-snapshot deduplication.
- Manifests do not enumerate and hash protected content.
- A successful upload does not prove that a recovery will work.
- Restore downloads data but does not reconstruct or validate provider state.
- The implementation is tied to AWS S3.
- The installed multi-provider behavior is not fully represented by the public
  repository, making maintenance and audit harder.

## Migration plan

1. Keep the current timer enabled.
2. Point Akeep at a new prefix or target; never reuse or mutate the old prefix.
3. Run both systems in parallel for at least 14 days.
4. Compare discovered source coverage and logical file manifests.
5. Perform current and historical recovery drills from Akeep.
6. Inject corruption into a copied Akeep archive and verify detection.
7. Disable the old timer only after the gate in
   [mvp.md](mvp.md#6-replacement-and-dogfood-gate) passes.
8. Keep the old remote data as a fallback through at least one more successful
   Akeep restore drill.

## Replacement boundary

Passing the gate means Akeep can replace this dedicated agent-history backup
service for our own use. It does not mean Akeep replaces:

- provider-native resume and checkpoint features;
- Git repositories and worktrees;
- a general home-directory or system backup;
- artifact storage outside configured provider paths.
