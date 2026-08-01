# Akeep

[![CI](https://github.com/eunomia-bpf/akeep/actions/workflows/ci.yml/badge.svg)](https://github.com/eunomia-bpf/akeep/actions/workflows/ci.yml)
[![Security audit](https://github.com/eunomia-bpf/akeep/actions/workflows/audit.yml/badge.svg)](https://github.com/eunomia-bpf/akeep/actions/workflows/audit.yml)

**Privacy-first, Git-like backup, recovery, migration, and sharing for AI agent session history.**

Your agent history is not a cache.

Akeep is a privacy-first CLI that discovers coding-agent sessions and saves
them as compressed, deduplicated commits. You can inspect history, compare two
versions, check archive integrity, restore provider-native files, and clone the
repository. Everything works locally; S3-compatible storage and age encryption
are optional.

## Why Akeep

Coding-agent histories routinely contain hours of decisions, commands, edits,
artifacts, and private code. Provider-local files are useful but undocumented,
mutable, and not a backup. For example, Claude Code documents a
[30-day default cleanup period](https://code.claude.com/docs/en/sessions) for
local transcripts.

Akeep is designed around five promises:

- **Versioned:** commits have messages and parent links; `HEAD~N`, `log`, and
  `diff` make history understandable.
- **Cloud backup:** keep history locally or back it up to AWS S3 and
  S3-compatible storage such as Cloudflare R2.
- **Exact:** raw provider files remain the source of truth and are preserved
  byte-for-byte.
- **Private:** no upload or telemetry happens by default. Client-side
  encryption is available but never forced.
- **Non-destructive:** commits never edit provider state, and checkout defaults
  to a separate scratch directory.

## Install

Install from crates.io:

```console
cargo install akeep
```

On Linux or macOS, you can instead download the latest published binary with
one command:

```console
curl -fsSL https://raw.githubusercontent.com/eunomia-bpf/akeep/main/scripts/install.sh | sh
```

The installer detects x86_64/ARM64 and writes to `~/.local/bin` unless
`AKEEP_INSTALL_DIR` is set. Run it again whenever you want to install the
newest published release. You can also download and verify an archive directly
from [GitHub Releases](https://github.com/eunomia-bpf/akeep/releases).

Rerun your chosen installation command to update. Akeep does not update itself
silently.

## Usage

Initialize a repository and exercise the core version-history loop:

```console
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

[Cloudflare R2](https://developers.cloudflare.com/r2/api/s3/api/) uses the same
S3-compatible path. Configure an AWS CLI profile with R2 credentials and set
its account endpoint:

```console
akeep init \
  --s3-bucket my-r2-bucket \
  --s3-prefix akeep/my-machine \
  --aws-profile r2 \
  --s3-endpoint-url https://ACCOUNT_ID.r2.cloudflarestorage.com \
  --encryption age
```

The Linux scheduler installs one service and timer per vault under the systemd
user-unit directory. It is persistent across downtime, adds a randomized
six-hour delay, runs with low CPU/I/O priority, and uses the same per-vault lock
as manual commits. Uninstalling it leaves configuration and archives untouched:

```console
akeep schedule uninstall
```

If upgrading from a build whose generated service invoked the former `backup`
command, reinstall the timer immediately after replacing the binary:

```console
akeep schedule install --weekly
```

See [configuration and operations](docs/configuration.md) and the
[archive format](docs/archive-format.md). Cross-agent handoff is described in
[the semantic handoff workflow](docs/portable-history.md).

## What Akeep is not

- It is not a general computer backup. Keep using Git and a normal system
  backup.
- It is not a chat-memory or RAG product.
- It does not promise lossless conversion between undocumented provider session
  formats.
- It does not delete or offload live provider data.
- It is not coupled to any observability or analysis product.

## Move from an existing backup

Akeep can be used as the primary backup for supported agent histories. When
migrating from another backup, keep both running until Akeep has completed
several scheduled commits and you have recovered both a current and an older
commit. This avoids creating a coverage gap while changing backup systems.

Our own installation protects more than 50 GB across five agent providers. A
full recovery reproduced every archived file and byte, recovered SQLite
databases passed integrity checks, and the bounded pipeline reduced observed
peak memory from 23.6 GiB to about 243 MiB. The dated evidence and conservative
migration checklist are documented separately.

See:

- [Configuration and operations](docs/configuration.md)
- [Provider compatibility matrix](docs/providers.md)
- [Recovery and rollback runbook](docs/recovery-runbook.md)
- [Testing and reliability evidence](docs/testing.md)
- [Archive format](docs/archive-format.md)
- [Product overview (中文)](docs/product.zh-CN.md)
- [Security policy](SECURITY.md)
- [Contributing](CONTRIBUTING.md)

## Supported today

Akeep provides the complete local and S3-compatible versioned-backup loop:
provider discovery, bounded streaming commits, compression and deduplication,
optional age encryption, history and diffs, full integrity checks, exact
scratch recovery, repository cloning, and Linux scheduled commits. See the
compatibility matrix for the provider data included by each adapter.

## License

[MIT](LICENSE)
