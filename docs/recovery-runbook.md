# Recovery and rollback runbook

This runbook is for a vault operator. Commands use placeholders deliberately;
do not paste private bucket names or identities into issues.

## Routine drill

```console
akeep doctor
akeep snapshots
akeep verify latest
akeep recover latest --to /tmp/akeep-drill
# Smaller provider-native drill:
akeep recover latest --provider claude-code --to /tmp/akeep-claude-drill
```

Compare recovered file BLAKE3 hashes with the JSON export or manifest. Open
recovered SQLite files with `PRAGMA integrity_check`. Keep the scratch directory
separate from live provider homes.

## Interrupted backup

An interrupted backup may leave immutable unreferenced objects, but it publishes
no manifest and does not update `refs/latest`. Rerun `akeep backup`; content
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
enforce mode 0600 on Unix, and run `akeep doctor` followed by `akeep verify`.
There is no cryptographic bypass if all matching identities are lost.

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
2. Record the affected recovery-point IDs and local verification receipts.
3. Preserve bucket versions and access logs.
4. Recover a known historical manifest by explicit ID, not `latest`.
5. Run full verification into a new scratch directory.
6. Rotate storage credentials if exposure is suspected.
7. For a plaintext vault, assume archive contents were readable. For an age
   vault, protect and rotate access credentials while preserving the identity
   needed for recovery.
