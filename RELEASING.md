# Releasing Akeep

Releases are irreversible registry and GitHub operations. Start from a clean,
green `main`, confirm `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, README install
commands, and the proposed `v<package-version>` tag agree.

Run the local release gate:

```console
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --locked
cargo publish --locked --dry-run
```

The first crates.io publication must use a maintainer token. Publish it before
the Git tag so the tag workflow's registry install test can observe it:

```console
cargo publish --locked
git tag -a v0.1.0-alpha.1 -m "Akeep v0.1.0-alpha.1"
git push origin v0.1.0-alpha.1
```

The tag workflow builds and attests four native archives, creates a GitHub
prerelease with `SHA256SUMS` and `install.sh`, installs every public binary on
its matching runner, and installs the exact version from crates.io. A release
is complete only after those jobs pass and both public install paths are
smoke-tested outside the source checkout.
