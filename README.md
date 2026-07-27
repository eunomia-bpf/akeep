# Akeep

**Private, verified backup and recovery for coding-agent work.**

Your agent history is not a cache.

Akeep is a local-first CLI for preserving coding-agent sessions and related
state. It creates compressed, content-addressed recovery points, encrypts data
before it reaches remote storage, verifies what was written, and recovers the
provider-native files without silently overwriting live state.

> [!IMPORTANT]
> Akeep is currently in design/pre-alpha. It is not yet protecting your data.
> The first implementation milestone is a dogfoodable Linux CLI, not a GUI or
> hosted service.

## Why Akeep

Coding-agent histories routinely contain hours of decisions, commands, edits,
artifacts, and private code. Provider-local files are useful but undocumented,
mutable, and not a backup.

Akeep is designed around five promises:

- **Local-first:** no account, network request, or telemetry is required.
- **Exact:** raw provider files remain the source of truth and are preserved
  byte-for-byte.
- **Private:** remote objects are encrypted on the client; storage providers do
  not receive plaintext.
- **Verifiable:** every recovery point has a versioned manifest and content
  hashes.
- **Non-destructive:** backup never edits provider state, and recovery defaults
  to a scratch directory.

## The first usable release

The v0.1 release is intentionally a backup-and-recovery product, not another
history viewer.

It will:

- discover raw history from Claude Code, Codex CLI, Grok CLI, Kimi Code, and
  OpenCode;
- take consistent snapshots of live SQLite databases;
- create incremental, deduplicated, compressed archives;
- encrypt archives before sending them to a remote target;
- write to a local directory or an S3-compatible object store;
- list, inspect, verify, and recover historical recovery points;
- install an optional weekly systemd user timer on Linux;
- preserve deleted or superseded local sessions in the archive.

The planned CLI contract is:

```console
akeep doctor
akeep backup
akeep snapshots
akeep verify latest
akeep recover latest --to /tmp/akeep-recovery
akeep schedule install --weekly
```

Cross-agent semantic handoff, local search, additional storage backends, and a
desktop UI come after verified recovery works on real data.

## What Akeep is not

- It is not a general computer backup. Keep using Git and a normal system
  backup.
- It is not a chat-memory or RAG product.
- It does not promise lossless conversion between undocumented provider session
  formats.
- It does not delete or offload live provider data in v0.1.
- It is not coupled to any observability or analysis product.

## Can it replace an existing backup script?

Eventually, yes—but only after evidence.

Our initial dogfood machine already has more than 50 GB of agent state and a
working weekly S3 backup service. Akeep will run beside that service until it
passes the replacement gate: repeated automatic backups, recovery of both
current and older snapshots, byte-level verification, corruption detection,
and a provider-level restore smoke test. The old service stays enabled until
those checks pass.

See:

- [MVP specification](docs/mvp.md)
- [Current backup baseline](docs/current-backup-baseline.md)
- [Product and launch plan (中文)](docs/product.zh-CN.md)

## Project status

The repository currently contains the product contract and implementation
acceptance criteria. The next milestone is a minimal Rust CLI that can run
`akeep doctor`, create a local recovery point, verify it, and recover it into a
scratch directory.

## License

[MIT](LICENSE)
