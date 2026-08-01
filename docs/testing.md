# Testing and reliability evidence

Akeep counts an integrity-checked restore—not an upload—as success. This page
separates repeatable software tests from the time-based migration safety gate
for our previous backup service.

## Automated suite

Run the same checks used by CI:

```console
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --locked
cargo package --locked
```

CI runs these checks on current Ubuntu and macOS, plus a dedicated build on the
declared Rust 1.85 MSRV. Synthetic fixtures never contain real sessions,
credentials, bucket names, identities, or home paths.

The suite covers:

- five-provider discovery and known credential/cache exclusions;
- live SQLite backup during concurrent WAL writes and recovered
  `PRAGMA integrity_check`;
- early staging of rotating provider files that disappear before archival;
- local, plaintext S3-compatible, and age-encrypted round trips;
- commit messages, parent chains, `HEAD~N`, and add/modify/delete diffs;
- exact filesystem/S3 repository clones, encrypted-key non-copying, overlap
  refusal, and incomplete-clone markers;
- content deduplication within a chunk batch, across parallel files, and across
  commits;
- interrupted remote upload with no manifest publication, followed by a
  successful retry;
- missing, corrupt, and reordered object rejection;
- non-empty, symlinked, and vault-overlapping recovery target rejection;
- incomplete recovery markers and byte/hash validation;
- provider-scoped recovery without a false whole-snapshot integrity receipt;
- bounded parallel archive checks and recovery with ordered
  reconstruction of multi-chunk files;
- 32 MiB multi-chunk scale and incremental rerun behavior;
- systemd ownership, rollback, escaping, persistence, and uninstall behavior;
- reviewed Claude Code ↔ Codex semantic handoff.

The fake S3 CLI exercises the same process boundary and object contract as the
real backend while keeping CI offline and deterministic. It also asserts that a
multi-chunk commit uses one recursive object-batch upload instead of one AWS CLI
process per chunk.

The release workflow builds native Linux x86_64/ARM64 and macOS Intel/Apple
Silicon archives, attests them, publishes checksums, downloads each public
artifact on its matching architecture, and runs `init`, `commit`, and `fsck`.
It separately installs the tagged version from crates.io.

For a short end-to-end demonstration:

```console
cargo build
AKEEP_BIN=target/debug/akeep ./scripts/demo.sh
```

The script creates only synthetic data in a mode-0700 temporary directory,
compares every recovered fixture byte, deliberately corrupts a temporary
archive object, and requires `fsck` to reject it.

## Manual recovery drill

For an ordinary machine:

```console
akeep status
akeep commit -m "manual recovery drill"
akeep log
akeep fsck HEAD
akeep checkout HEAD --to /path/on/a-separate-disk/akeep-drill
```

If scratch space is limited, recover one provider:

```console
akeep checkout HEAD \
  --provider claude-code \
  --to /tmp/akeep-claude-drill
```

The filtered drill verifies every selected object and file, but deliberately
does not record the entire commit as fully checked.

## Migration safety gate

The checklist in [the reliability specification](reliability.md) is
intentionally stricter than CI. It requires three scheduled runs across at
least 14 days, current and week-old full restores, corruption rejection, and
provider-native recognition. The previous backup remains enabled until every
unchecked item passes. This is a conservative migration policy for our own
installation, not a restriction on using Akeep. The checklist records dated,
non-secret evidence; bucket names, account details, source paths, and session
content must never be committed.

## Performance baseline

The optimized 2026-08-01 real S3 commit covered 53,259 files and
60,730,326,091 logical bytes. With four workers and staged recursive uploads it
added 391 objects (165,347,103 stored bytes), used 249,128 KiB peak RSS, 243.7
CPU seconds, and 236.4 wall-clock seconds. An earlier full run of the old
pipeline reached a 23.6 GiB process-tree peak; its later 56.3 GB incremental run
used 667.6 CPU seconds and 510.8 wall-clock seconds. These are real successive
workloads rather than a controlled microbenchmark, but they establish the
operational resource envelope. Archive format, chunk hashes, compression,
encryption, S3 keys, and recovery semantics did not change.
