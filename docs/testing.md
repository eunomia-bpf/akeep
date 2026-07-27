# Testing and reliability evidence

Akeep counts a verified recovery—not an upload—as success. This page separates
repeatable software tests from the time-based dogfood replacement gate.

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
- content deduplication within a chunk batch, across parallel files, and across
  recovery points;
- interrupted remote upload with no manifest publication, followed by a
  successful retry;
- missing, corrupt, and reordered object rejection;
- non-empty, symlinked, and vault-overlapping recovery target rejection;
- incomplete recovery markers and byte/hash validation;
- provider-scoped recovery without a false whole-snapshot verification receipt;
- bounded parallel archive, verification, and recovery with ordered
  reconstruction of multi-chunk files;
- 32 MiB multi-chunk scale and incremental rerun behavior;
- systemd ownership, rollback, escaping, persistence, and uninstall behavior;
- search, exact JSON/base64 export, Markdown export, and semantic handoff.

The fake S3 CLI exercises the same process boundary and object contract as the
real backend while keeping CI offline and deterministic.

For a short end-to-end demonstration:

```console
cargo build
AKEEP_BIN=target/debug/akeep ./scripts/demo.sh
```

The script creates only synthetic data in a mode-0700 temporary directory,
compares every recovered fixture byte, deliberately corrupts a temporary
archive object, and requires `verify` to reject it.

## Manual recovery drill

For an ordinary machine:

```console
akeep doctor
akeep backup
akeep snapshots
akeep verify latest
akeep recover latest --to /path/on/a-separate-disk/akeep-drill
```

If scratch space is limited, recover one provider:

```console
akeep recover latest \
  --provider claude-code \
  --to /tmp/akeep-claude-drill
```

The filtered drill verifies every selected object and file, but deliberately
does not claim the entire recovery point was fully verified.

## Dogfood replacement gate

The public checklist in [the MVP specification](mvp.md) is intentionally
stricter than CI. It requires three scheduled runs across at least 14 days,
current and week-old full restores, corruption rejection, and provider-native
recognition. The pre-existing backup remains enabled until every unchecked item
passes. Akeep's code can be release-ready before enough wall-clock evidence
exists to replace that service.
