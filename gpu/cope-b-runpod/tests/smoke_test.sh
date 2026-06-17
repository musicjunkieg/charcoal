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
for _ in $(seq 1 60); do
    if curl -sf http://localhost:8000/v1/models >/dev/null; then
        break
    fi
    sleep 5
done

POLICY=$(cat "$POLICY_PATH")
FAIL=0

# Build the POLICY/CONTENT body the same way prompt.build_body does so the
# raw-completions and chat-completions modes share a body. The chat path then
# wraps the body in the Gemma role markers via the server's chat template;
# the completions path sends the body as a plain prompt.
build_body() {
    local content="$1"
    printf 'Examine the given POLICY and determine if the given CONTENT meets the criteria for ANY of the LABELS. Answer "1" if yes, and "0" if no.\n\n\nPOLICY\n======\n\n%s\n\n\nCONTENT\n=======\n\n%s' "$POLICY" "$content"
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
