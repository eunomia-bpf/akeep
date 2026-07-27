#!/usr/bin/env bash
set -euo pipefail

AKEEP_DEMO_BIN=${AKEEP_BIN:-akeep}
AKEEP_DEMO_ROOT=$(mktemp -d)
AKEEP_DEMO_CONFIG="$AKEEP_DEMO_ROOT/config.toml"
AKEEP_DEMO_VAULT="$AKEEP_DEMO_ROOT/vault"
AKEEP_DEMO_RECOVERY="$AKEEP_DEMO_ROOT/recovery"
AKEEP_DEMO_CLONE="$AKEEP_DEMO_ROOT/clone"

cleanup_demo() {
    if [ -n "${AKEEP_DEMO_ROOT:-}" ] && [ -d "$AKEEP_DEMO_ROOT" ]; then
        find "$AKEEP_DEMO_ROOT" -depth -delete
    fi
}
trap cleanup_demo EXIT

run_akeep() {
    env \
        CLAUDE_CONFIG_DIR="$AKEEP_DEMO_ROOT/claude" \
        CODEX_HOME="$AKEEP_DEMO_ROOT/codex" \
        GROK_HOME="$AKEEP_DEMO_ROOT/grok" \
        KIMI_CODE_HOME="$AKEEP_DEMO_ROOT/kimi" \
        OPENCODE_SHARE_DIR="$AKEEP_DEMO_ROOT/opencode-share" \
        OPENCODE_STATE_DIR="$AKEEP_DEMO_ROOT/opencode-state" \
        "$AKEEP_DEMO_BIN" --config "$AKEEP_DEMO_CONFIG" "$@"
}

mkdir -p \
    "$AKEEP_DEMO_ROOT/claude/projects/demo" \
    "$AKEEP_DEMO_ROOT/codex/sessions" \
    "$AKEEP_DEMO_ROOT/grok/sessions" \
    "$AKEEP_DEMO_ROOT/kimi/sessions" \
    "$AKEEP_DEMO_ROOT/opencode-share/storage" \
    "$AKEEP_DEMO_ROOT/opencode-state"

printf '%s\n' '{"role":"user","content":"preserve this decision"}' \
    > "$AKEEP_DEMO_ROOT/claude/projects/demo/session.jsonl"
printf '%s\n' '{"type":"message","text":"keep the migration reversible"}' \
    > "$AKEEP_DEMO_ROOT/codex/sessions/session.jsonl"
printf '%s\n' '{"message":"grok fixture"}' \
    > "$AKEEP_DEMO_ROOT/grok/sessions/session.jsonl"
printf '%s\n' '{"message":"kimi fixture"}' \
    > "$AKEEP_DEMO_ROOT/kimi/sessions/session.jsonl"
printf '%s\n' '{"message":"opencode fixture"}' \
    > "$AKEEP_DEMO_ROOT/opencode-share/storage/session.json"

printf '%s\n' '==> Initialize a local plaintext demo vault'
"$AKEEP_DEMO_BIN" --config "$AKEEP_DEMO_CONFIG" init --target "$AKEEP_DEMO_VAULT"

printf '%s\n' '==> Discover five providers'
run_akeep status

printf '%s\n' '==> Commit two versions, diff, check, and restore'
run_akeep commit -m "initial synthetic history"
printf '%s\n' '{"role":"assistant","content":"the decision is now versioned"}' \
    >> "$AKEEP_DEMO_ROOT/claude/projects/demo/session.jsonl"
run_akeep commit -m "version the follow-up"
run_akeep log
run_akeep diff HEAD~1 HEAD --name-only
run_akeep fsck HEAD
run_akeep checkout HEAD --to "$AKEEP_DEMO_RECOVERY"

cmp \
    "$AKEEP_DEMO_ROOT/claude/projects/demo/session.jsonl" \
    "$AKEEP_DEMO_RECOVERY/claude-code/projects/demo/session.jsonl"
cmp \
    "$AKEEP_DEMO_ROOT/codex/sessions/session.jsonl" \
    "$AKEEP_DEMO_RECOVERY/codex/sessions/session.jsonl"
cmp \
    "$AKEEP_DEMO_ROOT/grok/sessions/session.jsonl" \
    "$AKEEP_DEMO_RECOVERY/grok/sessions/session.jsonl"
cmp \
    "$AKEEP_DEMO_ROOT/kimi/sessions/session.jsonl" \
    "$AKEEP_DEMO_RECOVERY/kimi-code/sessions/session.jsonl"
cmp \
    "$AKEEP_DEMO_ROOT/opencode-share/storage/session.json" \
    "$AKEEP_DEMO_RECOVERY/opencode/storage/session.json"
printf '%s\n' 'PASS: recovered provider-native files match every source byte'

printf '%s\n' '==> Clone the repository and check the independent bundle'
run_akeep clone "$AKEEP_DEMO_CLONE"
"$AKEEP_DEMO_BIN" --config "$AKEEP_DEMO_CLONE/config.toml" fsck HEAD
printf '%s\n' 'PASS: cloned repository has a readable, complete HEAD'

AKEEP_DEMO_OBJECTS=0
while IFS= read -r -d '' object; do
    printf '%s' 'deliberately corrupt' > "$object"
    AKEEP_DEMO_OBJECTS=$((AKEEP_DEMO_OBJECTS + 1))
done < <(find "$AKEEP_DEMO_VAULT/objects" -type f -print0)
test "$AKEEP_DEMO_OBJECTS" -gt 0
if run_akeep fsck HEAD >/dev/null 2>&1; then
    printf '%s\n' 'FAIL: corrupted archive passed fsck' >&2
    exit 1
fi
printf '%s\n' 'PASS: full fsck rejected a deliberately corrupted object'
