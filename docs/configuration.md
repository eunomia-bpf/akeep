# Configuration and operations

Akeep writes a private TOML configuration at
`~/.config/akeep/config.toml` by default. Pass `--config FILE` before the
subcommand to operate another vault. Paths created by Akeep use mode 0700 for
directories and 0600 for sensitive files on Unix.

## Local vault

```console
akeep init
# or
akeep init --target /mnt/backup/akeep
```

The default archive is `~/.local/share/akeep/vaults/default`. A small state
directory beside the archive holds only locks and temporary SQLite snapshots.
Do not place either path inside a provider directory.

## S3-compatible vault

AWS CLI v2 must be installed and able to access the bucket:

```console
aws s3api list-objects-v2 --bucket my-backups --max-keys 1
akeep init \
  --s3-bucket my-backups \
  --s3-prefix akeep/workstation \
  --aws-region us-west-2 \
  --aws-profile backup
```

At initialization, Akeep resolves and stores the absolute AWS CLI executable so
scheduled jobs do not depend on an interactive shell's `PATH`. Use
`--aws-cli /absolute/path/to/aws` to choose it explicitly and
`--s3-endpoint-url URL` for a compatible service.

Every vault needs its own prefix. Akeep never deletes remote objects. Bucket
versioning is recommended because `refs/latest` is intentionally updated after
each complete manifest; `akeep status` reports the versioning state when the
account can read it.

## Encryption

Encryption is a vault-level choice:

```console
# Default: no client-side encryption
akeep init

# Client-side authenticated age encryption
akeep init --encryption age
```

Age mode generates an X25519 recovery identity beside the configuration unless
`--age-identity-file FILE` selects an existing identity. Copy that file to a
separate password manager, encrypted removable disk, or offline backup. Losing
every copy makes the archive permanently unrecoverable. Do not put the only
copy inside the vault it unlocks.

Plaintext mode is fully supported. It is often appropriate for a trusted,
already-encrypted local disk. For a remote vault it means the storage operator
can read sessions; initialization and `status` say so explicitly.

Encryption mode is recorded in `vault.json` and cannot be switched in place.
Create a new vault to change it.

## Provider overrides

The `[sources]` section can override discovery roots:

```toml
[sources]
claude_home = "/home/me/.claude"
codex_home = "/home/me/.codex"
grok_home = "/home/me/.grok"
kimi_home = "/home/me/.kimi-code"
opencode_share = "/home/me/.local/share/opencode"
opencode_state = "/home/me/.local/state/opencode"
```

Run `akeep status` after editing. Its JSON form is suitable for monitoring:

```console
akeep status --json
```

For temporary or isolated runs, Akeep also honors provider environment
variables. Explicit `[sources]` values take precedence. Claude Code uses
`CLAUDE_CONFIG_DIR`; the older `CLAUDE_HOME` alias remains accepted for Akeep
compatibility. Codex uses `CODEX_HOME`; the other adapters accept `GROK_HOME`,
`KIMI_CODE_HOME`, `OPENCODE_SHARE_DIR`, and `OPENCODE_STATE_DIR`.

## Everyday version history

Akeep automatically discovers the supported provider files. There is no
required staging step or `add` command:

```console
akeep status
akeep commit -m "before upgrading the agent"
# Work normally, then save another version.
akeep commit -m "after the upgrade"
akeep log
akeep diff HEAD~1 HEAD
```

Commit messages are optional, limited to one short line, and stored inside the
manifest (encrypted together with it in age mode). `HEAD` means the newest
complete commit; `HEAD~N` follows parent links.

## Automatic commits on Linux

```console
akeep schedule install --weekly
akeep schedule status
systemctl --user list-timers 'akeep-*'
journalctl --user -u 'akeep-*.service'
```

The generated timer uses `Persistent=true`, so a missed run starts after the
user manager returns. A six-hour randomized delay prevents synchronized
uploads. The service uses low CPU and idle I/O priority.

Pre-alpha services generated before the Git-like command change invoke
`backup`. After replacing that binary, run `akeep schedule install --weekly`
again to atomically regenerate and reactivate the service with `commit`.

To remove automation without touching data:

```console
akeep schedule uninstall
```

## Recovery drill

Successful upload is not proof of recovery. Regularly run:

```console
akeep log
akeep fsck HEAD
akeep checkout HEAD --to /tmp/akeep-drill
# Smaller provider-native drill when scratch space is limited:
akeep checkout HEAD --provider claude-code --to /tmp/akeep-claude-drill
```

`fsck` performs a full download/decrypt/decompress/hash pass by default.
`fsck --quick` checks manifest structure, object presence, and stored sizes.
Checkout accepts only a new or empty real directory, never follows a target
symlink, writes an incomplete marker until all files pass hashes, and never
modifies live provider directories. A provider-filtered recovery validates all
selected bytes but does not mark the whole commit as fully checked.

## Clone a repository

Copy the active filesystem or S3 repository into a new local bundle:

```console
akeep clone /mnt/backup/akeep-copy
akeep --config /mnt/backup/akeep-copy/config.toml log
akeep --config /mnt/backup/akeep-copy/config.toml fsck HEAD
```

The destination must not exist and must not overlap the source repository or
state directory. Akeep copies every stored object, checks its transport hash,
then walks and checks the cloned commit chain. An interrupted clone keeps
`.akeep-clone-incomplete`. For age encryption, copy the identity separately;
the clone intentionally contains no private key.
