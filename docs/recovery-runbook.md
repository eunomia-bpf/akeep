# Recovery and rollback runbook

This runbook is for a vault operator. Commands use placeholders deliberately;
do not paste private bucket names or identities into issues.

## Routine drill

```console
akeep status
akeep log
akeep fsck HEAD
akeep checkout HEAD --to /tmp/akeep-drill
# Smaller provider-native drill:
akeep checkout HEAD --provider claude-code --to /tmp/akeep-claude-drill
```

Confirm `akeep fsck` succeeds and compare recovered file BLAKE3 hashes with the
manifest. Open recovered SQLite files with `PRAGMA integrity_check`. Keep the
scratch directory separate from live provider homes.

To confirm that current provider binaries recognize the restored native state,
open their local resume pickers against the scratch copy and exit without
starting a model turn:

```console
# Run from the original project directory represented by the restored session.
CLAUDE_CONFIG_DIR=/tmp/akeep-drill/claude-code claude --resume

# --all avoids filtering the picker by the current working directory.
CODEX_HOME=/tmp/akeep-drill/codex codex resume --all
```

Do not point these variables at the live provider homes during a drill. A
provider may update indexes inside the scratch copy. Recognition means a known
restored session appears in the native picker; it does not require sending a
prompt or making a network request.

## Interrupted commit

An interrupted commit may leave immutable unreferenced objects, but it publishes
no manifest and does not update `refs/latest`. Rerun `akeep commit`; content
addressing safely reuses complete objects. Do not manually delete remote
objects.

## Interrupted recovery

The target retains `.akeep-recovery-incomplete`. Treat every file there as
untrusted and incomplete. Preserve it for diagnosis if needed, then choose a
new empty scratch directory for the retry. Never point recovery at a live
provider home.

## Missing age identity

Stop. Do not edit `vault.json`, the configured recipient, encrypted objects, or
manifests. Restore the exact matching identity from the offline key backup,
enforce mode 0600 on Unix, and run `akeep status` followed by `akeep fsck`.
There is no cryptographic bypass if all matching identities are lost.

## Interrupted clone

An interrupted or rejected clone keeps `.akeep-clone-incomplete` in its new
destination. Do not use that bundle as a repository. Preserve it for diagnosis
or remove the whole new directory, then rerun `akeep clone` with another
nonexistent destination. An encrypted clone deliberately does not contain the
age identity; restore that key separately before running `fsck` or `checkout`.

## Scheduler rollback

```console
akeep schedule status
akeep schedule uninstall
```

This removes only Akeep's generated user units. Configuration and archives
remain. If Akeep is still in shadow mode, confirm the previous timer remains
enabled and active:

```console
systemctl --user enable --now claude-codex-sync-aws.timer
systemctl --user status claude-codex-sync-aws.timer
```

Do not disable the previous timer until every replacement checkbox in
[mvp.md](mvp.md#6-replacement-and-dogfood-gate) passes.

## Remote incident

1. Stop the Akeep timer without deleting data.
2. Record the affected commit IDs and local integrity-check receipts.
3. Preserve bucket versions and access logs.
4. Check out a known historical manifest by explicit ID, not `HEAD`.
5. Run full `fsck`, then checkout into a new scratch directory.
6. Rotate storage credentials if exposure is suspected.
7. For a plaintext vault, assume archive contents were readable. For an age
   vault, protect and rotate access credentials while preserving the identity
   needed for recovery.
