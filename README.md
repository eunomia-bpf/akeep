# Akeep

[![CI](https://github.com/eunomia-bpf/akeep/actions/workflows/ci.yml/badge.svg)](https://github.com/eunomia-bpf/akeep/actions/workflows/ci.yml)
[![Security audit](https://github.com/eunomia-bpf/akeep/actions/workflows/audit.yml/badge.svg)](https://github.com/eunomia-bpf/akeep/actions/workflows/audit.yml)

**Git-like version history for coding-agent work.**

Your agent history is not a cache.

Akeep is a privacy-first CLI that discovers coding-agent sessions and saves
them as compressed, deduplicated commits. You can inspect history, compare two
versions, check archive integrity, restore provider-native files, and clone the
repository. Everything works locally; S3-compatible storage and age encryption
are optional.

> [!IMPORTANT] The complete versioned-backup loop works with
> local and S3-compatible targets, optional age encryption, full integrity
> checks, scratch checkout, repository cloning, and a Linux systemd user timer.
> Keep an existing backup during the documented shadow-run gate; software
> passing tests is not yet the same as a proven long-term recovery history.

## Why Akeep

Coding-agent histories routinely contain hours of decisions, commands, edits,
artifacts, and private code. Provider-local files are useful but undocumented,
mutable, and not a backup. For example, Claude Code documents a
[30-day default cleanup period](https://code.claude.com/docs/en/sessions) for
local transcripts.

Akeep is designed around five promises:

- **Versioned:** commits have messages and parent links; `HEAD~N`, `log`, and
  `diff` make history understandable.
- **Local-first:** no account, network request, or telemetry is required.
- **Exact:** raw provider files remain the source of truth and are preserved
  byte-for-byte.
- **Private:** no upload or telemetry happens by default. Client-side
  encryption is available but never forced.
- **Non-destructive:** commits never edit provider state, and checkout defaults
  to a separate scratch directory.

## Usage

Build and exercise the current local CLI:

```console
git clone https://github.com/eunomia-bpf/akeep
cd akeep
cargo install --path .

akeep init
akeep status
akeep commit -m "before the migration"
# Continue working with your agents, then:
akeep commit -m "after the migration"
akeep log
akeep diff HEAD~1 HEAD
akeep fsck HEAD
akeep checkout HEAD --to /tmp/akeep-recovery
akeep checkout HEAD --provider claude-code --to /tmp/akeep-claude-drill
akeep clone /mnt/backup/akeep-copy
akeep index rebuild
akeep search "database migration"
akeep export HEAD --format markdown --to recovery-point.md
akeep schedule install --weekly
akeep schedule status
```

`akeep init` writes `~/.config/akeep/config.toml` and creates a private local
repository under `~/.local/share/akeep/vaults/default`. Review `akeep status`
before the first commit. There is deliberately no required `add`: provider
adapters discover the supported durable files automatically. First-time setup
is `init`, `status`, `commit`; ordinary use can be only `commit`. Akeep skips
known credential, cache, and temporary paths and never follows symlinks.

`clone` copies the active repository—filesystem or S3—into
`DIRECTORY/{config.toml,repository/,state/}` and checks every transferred
object plus the cloned commit chain. Use the clone directly:

```console
akeep --config /mnt/backup/akeep-copy/config.toml log
akeep --config /mnt/backup/akeep-copy/config.toml fsck HEAD
```

For an encrypted repository, the clone keeps the configured identity path but
does not copy the private age identity. Move or back up that key separately.

Run the self-contained trust demo with synthetic five-provider fixtures:

```console
cargo build
AKEEP_BIN=target/debug/akeep ./scripts/demo.sh
```

It proves a byte-identical checkout, then corrupts its temporary archive and
proves that a full integrity check rejects it. The demo uses a private temporary
directory and removes it on exit.

Encryption remains optional. To create an age-encrypted vault:

```console
akeep init --encryption age
```

Akeep generates a mode-0600 recovery identity beside the configuration and
prints its path. Back it up separately: if every copy is lost, nobody can
decrypt the repository. `akeep status` performs an encrypt/decrypt self-test
whenever encryption is enabled.

Use an S3-compatible target by supplying a bucket and an isolated prefix:

```console
akeep init \
  --s3-bucket my-backup-bucket \
  --s3-prefix akeep/my-machine \
  --aws-profile backup \
  --encryption age
```

Akeep invokes AWS CLI v2 without a shell, records its absolute executable path
in the configuration, checks bucket access and versioning, uploads immutable
objects before publishing a manifest, and never issues a remote delete.
`--s3-endpoint-url` supports S3-compatible services. Remote encryption is
recommended, not mandatory: omitting `--encryption age` produces a clear
warning and a fully supported plaintext vault.

The Linux scheduler installs one service and timer per vault under the systemd
user-unit directory. It is persistent across downtime, adds a randomized
six-hour delay, runs with low CPU/I/O priority, and uses the same per-vault lock
as manual commits. Uninstalling it leaves configuration and archives untouched:

```console
akeep schedule uninstall
```

If upgrading from an earlier pre-alpha build whose generated service invoked
`backup`, reinstall the timer immediately after replacing the binary:

```console
akeep schedule install --weekly
```

See [configuration and operations](docs/configuration.md) and the
[archive format](docs/archive-format.md). Search, export, and cross-agent
handoff are described in [portable history workflows](docs/portable-history.md).

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
passes the replacement gate: repeated automatic commits, recovery of both
current and older versions, byte-level integrity checks, corruption detection,
and a provider-level restore smoke test. The old service stays enabled until
those checks pass.

See:

- [MVP specification](docs/mvp.md)
- [Current backup baseline](docs/current-backup-baseline.md)
- [Provider compatibility matrix](docs/providers.md)
- [Product and launch plan (中文)](docs/product.zh-CN.md)
- [Recovery and rollback runbook](docs/recovery-runbook.md)
- [Testing and reliability evidence](docs/testing.md)
- [Security policy](SECURITY.md)
- [Contributing](CONTRIBUTING.md)

## Project status

The v0.1 versioned-backup feature set is implemented. The project is not ready to
replace the existing dogfood backup until the time-based shadow-run and recovery
drills in the replacement gate pass.

## License

[MIT](LICENSE)
