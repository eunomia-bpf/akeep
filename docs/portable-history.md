# Search, export, and semantic handoff

These workflows are derived from recovery points. Raw provider files remain the
source of truth, and all derived outputs can be deleted and rebuilt.

## Local search

```console
akeep index rebuild
akeep search "database migration"
akeep search "failing integration test" --limit 50 --json
```

The rebuild scans completed recovery points from newest to oldest and indexes
the newest archived version of every Claude Code and Codex text path. This
retains discoverability for session files deleted from the live provider while
avoiding one duplicate copy per recovery point. SQLite/binary artifacts are not
full-text indexed.

The FTS5 database lives at `<vault state>/search.sqlite3`, uses mode 0600 on
Unix, and is never uploaded. It is plaintext even when the archive uses age
encryption, because local interactive search must be fast. Delete it at any time
and run `akeep index rebuild` again. Search terms are treated as literal words,
not raw FTS syntax.

## Export

Readable Markdown:

```console
akeep export latest --format markdown --to session-export.md
```

Markdown includes verified UTF-8 text files up to 64 MiB each. It records but
omits SQLite, binary, invalid-UTF-8, and larger artifacts. Use it for review,
not exact recovery.

Exact JSON:

```console
akeep export latest --format json --to recovery-point.json
```

The JSON document includes the validated manifest and every file as a streaming
standard-base64 payload. It is exact but intentionally generic: Akeep does not
pretend undocumented provider formats share one lossless conversation schema.
Use `akeep recover` when provider-native files are desired directly.

Exports are private local files, are created with mode 0600 on Unix, cannot be
placed inside the vault/state directory, and never overwrite an existing file.
They contain session content in plaintext regardless of vault encryption.

## Claude Code ↔ Codex handoff

```console
akeep handoff latest \
  --from claude-code \
  --for codex \
  --goal "Finish the restore drill and fix any mismatch" \
  --decision "Keep remote encryption optional" \
  --open-task "Run the provider fixture smoke test" \
  --test-status "cargo test passes" \
  --repo . \
  --artifact target/recovery-report.json \
  --to handoff.md
```

The bundle explicitly separates two kinds of information:

- user-supplied goal, decisions, open tasks, and test status;
- captured Git branch/commit/status/diff statistics, artifact size/hash, and
  bounded tails from up to three recent archived source-agent text files.

Captured transcript tails may contain commands and results, but they are labeled
as evidence rather than presented as an AI-generated interpretation. The
receiving agent or human should review them. Artifacts must be regular files
inside the selected Git repository and are listed by relative path and BLAKE3
hash, not embedded.

Handoff currently supports Claude Code and Codex in either direction. It creates
a portable Markdown file; it does not write undocumented target-provider state
or claim lossless native import.
