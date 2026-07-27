#!/usr/bin/env bash
set -euo pipefail

AKEEP_DEMO_BIN=${AKEEP_BIN:-akeep}
AKEEP_DEMO_ROOT=$(mktemp -d)
AKEEP_DEMO_CONFIG="$AKEEP_DEMO_ROOT/config.toml"
AKEEP_DEMO_VAULT="$AKEEP_DEMO_ROOT/vault"
AKEEP_DEMO_RECOVERY="$AKEEP_DEMO_ROOT/recovery"

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
run_akeep doctor

printf '%s\n' '==> Back up, verify, and recover'
run_akeep backup
run_akeep snapshots
run_akeep verify latest
run_akeep recover latest --to "$AKEEP_DEMO_RECOVERY"

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

AKEEP_DEMO_OBJECT=$(find "$AKEEP_DEMO_VAULT/objects" -type f -print -quit)
test -n "$AKEEP_DEMO_OBJECT"
printf '%s' 'deliberately corrupt' > "$AKEEP_DEMO_OBJECT"
if run_akeep verify latest >/dev/null 2>&1; then
    printf '%s\n' 'FAIL: corrupted archive passed verification' >&2
    exit 1
fi
printf '%s\n' 'PASS: full verification rejected a deliberately corrupted object'
