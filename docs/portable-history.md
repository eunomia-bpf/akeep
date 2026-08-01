# Semantic handoff

This workflow is derived from a commit. Raw provider files remain the source of
truth, and the generated handoff can be deleted and rebuilt.

## Claude Code ↔ Codex handoff

```console
akeep handoff HEAD \
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
