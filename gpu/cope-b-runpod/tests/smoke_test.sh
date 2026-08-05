#!/usr/bin/env bash
# Local smoke test: serve the model under vLLM and walk fixture inputs.
# Default mode uses /v1/chat/completions (matches production handler's
# apply_chat_template path). Override with SMOKE_MODE=completions to use
# the raw /v1/completions endpoint with a hand-rolled body — useful when
# debugging whether the chat-template wrapper is hiding a problem.
#
# Requires a CUDA GPU (80 GB for full BF16). Apple Silicon will not run vllm.
# Run from gpu/cope-b-runpod/. Aborts after $MAX_FAILURES misses.
#
# Fixture: tests/fixtures/cope_b/smoke.jsonl (repo root), produced by the
# Chunk 2 A/B sample harvest. If it does not exist yet, this script aborts
# with a pointer rather than fabricating inputs.
set -euo pipefail

POLICY_PATH=${POLICY_PATH:-policy.txt}
MODEL=${MODEL:-zentropi-ai/cope-b-a4b}
SMOKE_MODE=${SMOKE_MODE:-chat}        # chat | completions
MAX_FAILURES=${MAX_FAILURES:-5}        # abort after N misses
FIXTURE=${FIXTURE:-../../tests/fixtures/cope_b/smoke.jsonl}

if [[ ! -f "$POLICY_PATH" ]]; then
    echo "ERROR: $POLICY_PATH not found"; exit 1
fi

if [[ ! -f "$FIXTURE" ]]; then
    echo "ERROR: fixture $FIXTURE not found."
    echo "       It is produced by the Chunk 2 A/B sample harvest"
    echo "       (find-fixtures tool). Generate it before running the smoke test."
    exit 1
fi

vllm serve "$MODEL" \
    --dtype bfloat16 \
    --max-model-len 4096 \
    --enable-prefix-caching \
    --port 8000 &
VLLM_PID=$!
trap "kill $VLLM_PID 2>/dev/null || true" EXIT

# Wait up to 5 min for the OpenAI-compatible endpoint to come up.
echo "Waiting for vLLM to start..."
READY=0
for _ in $(seq 1 60); do
    if curl -sf http://localhost:8000/v1/models >/dev/null; then
        READY=1
        break
    fi
    sleep 5
done
if [[ "$READY" != "1" ]]; then
    echo "ERROR: vLLM did not become ready on http://localhost:8000 within 5 minutes." >&2
    echo "       Check the vLLM server logs above for load/OOM errors." >&2
    exit 1
fi

POLICY=$(cat "$POLICY_PATH")
FAIL=0

# Build the POLICY/CONTENT body by calling the SAME builder production uses
# (prompt.build_body), rather than re-templating it in shell where it could
# silently drift from prompt.py and invalidate the production-parity guarantee.
# POLICY/CONTENT are passed via env to avoid shell-quoting a multi-line policy.
build_body() {
    local content="$1"
    SMOKE_POLICY="$POLICY" SMOKE_CONTENT="$content" python3 -c '
import os, sys
import prompt
sys.stdout.write(prompt.build_body(os.environ["SMOKE_POLICY"], os.environ["SMOKE_CONTENT"]))
'
}

classify() {
    local content="$1"
    local body request
    body=$(build_body "$content")
    if [[ "$SMOKE_MODE" == "chat" ]]; then
        request=$(jq -nc --arg model "$MODEL" --arg body "$body" \
            '{model: $model, max_tokens: 1, temperature: 0, messages: [{role: "user", content: $body}]}')
        curl -sf -X POST http://localhost:8000/v1/chat/completions \
            -H 'content-type: application/json' -d "$request" \
            | jq -r '.choices[0].message.content' | tr -d '[:space:]'
    else
        request=$(jq -nc --arg model "$MODEL" --arg prompt "$body" \
            '{model: $model, max_tokens: 1, temperature: 0, prompt: $prompt}')
        curl -sf -X POST http://localhost:8000/v1/completions \
            -H 'content-type: application/json' -d "$request" \
            | jq -r '.choices[0].text' | tr -d '[:space:]'
    fi
}

walk_fixture() {
    local fixture="$1"
    while IFS= read -r line; do
        local id expected content want verdict
        id=$(echo "$line" | jq -r .id)
        expected=$(echo "$line" | jq -r .label)
        content=$(echo "$line" | jq -r .content)
        case "$expected" in
            toxic)   want=1 ;;
            clean)   want=0 ;;
            *)       continue ;;   # skip uncertain
        esac
        verdict=$(classify "$content")
        if [[ "$verdict" != "$want" ]]; then
            echo "FAIL $id: expected $want got $verdict"
            FAIL=$((FAIL + 1))
            if [[ $FAIL -ge $MAX_FAILURES ]]; then
                echo "Aborting after $MAX_FAILURES failures"
                return $FAIL
            fi
        else
            echo "OK   $id"
        fi
    done < "$fixture"
}

walk_fixture "$FIXTURE"

echo "Mode: $SMOKE_MODE   Failures: $FAIL"
exit $FAIL
