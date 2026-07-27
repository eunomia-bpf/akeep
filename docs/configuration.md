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
each complete manifest; `akeep doctor` reports the versioning state when the
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
can read sessions; initialization and `doctor` say so explicitly.

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

Run `akeep doctor` after editing. Its JSON form is suitable for monitoring:

```console
akeep doctor --json
```

## Automatic backups on Linux

```console
akeep schedule install --weekly
akeep schedule status
systemctl --user list-timers 'akeep-*'
journalctl --user -u 'akeep-*.service'
```

The generated timer uses `Persistent=true`, so a missed run starts after the
user manager returns. A six-hour randomized delay prevents synchronized
uploads. The service uses low CPU and idle I/O priority.

To remove automation without touching data:

```console
akeep schedule uninstall
```

## Recovery drill

Successful upload is not proof of recovery. Regularly run:

```console
akeep snapshots
akeep verify latest
akeep recover latest --to /tmp/akeep-drill
```

`verify` performs a full download/decrypt/decompress/hash pass by default.
`verify --quick` checks manifest structure, object presence, and stored sizes.
Recovery accepts only a new or empty real directory, never follows a target
symlink, writes an incomplete marker until all files pass hashes, and never
modifies live provider directories.
