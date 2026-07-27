# Akeep

**Private, verified backup and recovery for coding-agent work.**

Your agent history is not a cache.

Akeep is a local-first CLI for preserving coding-agent sessions and related
state. It creates compressed, content-addressed recovery points, can encrypt
data before it reaches remote storage, verifies what was written, and recovers
the provider-native files without silently overwriting live state.

> [!IMPORTANT]
> Akeep is currently pre-alpha. Local backup, verification, and scratch recovery
> are implemented and tested. S3-compatible storage, optional encryption, and
> scheduling are still under development, so do not replace an existing backup
> yet.

## Why Akeep

Coding-agent histories routinely contain hours of decisions, commands, edits,
artifacts, and private code. Provider-local files are useful but undocumented,
mutable, and not a backup.

Akeep is designed around five promises:

- **Local-first:** no account, network request, or telemetry is required.
- **Exact:** raw provider files remain the source of truth and are preserved
  byte-for-byte.
- **Private:** no upload or telemetry happens by default. Client-side
  encryption is available but never forced.
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
- optionally encrypt archives before sending them to a remote target;
- write to a local directory or an S3-compatible object store;
- list, inspect, verify, and recover historical recovery points;
- install an optional weekly systemd user timer on Linux;
- preserve deleted or superseded local sessions in the archive.

Build and exercise the current local CLI:

```console
git clone https://github.com/eunomia-bpf/akeep
cd akeep
cargo install --path .

akeep init
akeep doctor
akeep backup
akeep snapshots
akeep verify latest
akeep recover latest --to /tmp/akeep-recovery
```

`akeep init` writes `~/.config/akeep/config.toml` and creates a private local
vault under `~/.local/share/akeep/vaults/default`. Review `akeep doctor` before
the first backup. Akeep skips known credential, cache, and temporary paths and
never follows symlinks.

Encryption remains optional. To create an age-encrypted vault:

```console
akeep init --encryption age
```

Akeep generates a mode-0600 recovery identity beside the configuration and
prints its path. Back it up separately: if every copy is lost, nobody can
decrypt the vault. `akeep doctor` performs an encrypt/decrypt self-test whenever
encryption is enabled.

S3-compatible storage, systemd scheduling, cross-agent semantic handoff, and
local search are the next implementation milestones.

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

The local recovery loop is implemented. The project is not ready to replace the
existing dogfood backup until remote storage, scheduling, and the documented
shadow-run recovery gate pass.

## License

[MIT](LICENSE)
