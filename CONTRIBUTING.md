# Contributing

Issues and focused pull requests are welcome. Akeep handles unusually sensitive
data, so reliability and privacy evidence matter more than feature count.

## Development

Rust 1.85 or newer is required.

```console
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo package
```

Tests must use synthetic fixtures and temporary directories. Never commit real
agent sessions, credentials, age identities, bucket/account names, absolute
home paths, or unredacted `status` output.

Changes to archive layout, manifests, encryption, exclusions, recovery
conflicts, or publication order need:

1. a failure-mode test;
2. backward-compatibility analysis;
3. corresponding format/runbook documentation;
4. an explicit statement of how incomplete work remains recoverable.

Provider adapters should stay read-only and narrowly include known durable
history. Adding broad home-directory traversal is not acceptable.

## Commit and review scope

Keep commits single-purpose and include the user-visible outcome in the message.
Pull requests should explain recovery and privacy impact, not only code shape.
Security-sensitive reports belong in a private advisory; see
[SECURITY.md](SECURITY.md).
