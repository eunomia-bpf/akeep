# Releasing Akeep

Releases are irreversible registry and GitHub operations. A successful `main`
CI run compares the version in `Cargo.toml` with crates.io and GitHub Releases.
When that version has not been published, CI publishes it automatically and
creates the matching `v<package-version>` release.

Run the local release gate:

```console
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --locked
cargo publish --locked --dry-run
```

Before merging a version bump, confirm `Cargo.toml`, `Cargo.lock`,
`CHANGELOG.md`, and README install commands agree, then run the local release
gate above. Do not create the tag or publish manually. The repository secret
`CARGO_REGISTRY_TOKEN` is available only to the trusted `main` publication job.

After the ordinary test and MSRV jobs pass, the release workflow builds and
attests four native archives, publishes the crate if absent, creates a stable
release or prerelease according to the SemVer suffix, and installs every public
artifact. If the Cargo version already exists in both public destinations, the
publication stage is a no-op. A release is complete only after the public
binary installer and crates.io install smoke tests pass.
