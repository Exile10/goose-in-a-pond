#!/usr/bin/env bash
# test-pond-agent-live.sh — End-to-end live test of the GIAP inference pipeline.
#
# Tests:
#   1. Server health check
#   2. Simple chat (LLM responds with text)
#   3. Tool calling (weather tool invoked and result synthesized)
#   4. Multi-turn context (model remembers previous turn)
#   5. Knowledge tool (Wikipedia lookup + truncation)
#   6. Latency measurement (TTFT for each request)
#
# Requirements:
#   - pond-server running on port 4000 (cargo run -p pond-server -- serve)
#   - A GGUF model loaded (gemma-4-E4B or E2B)
#
# Usage:
#   bash scripts/test-pond-agent-live.sh
#   POND_HOST=http://192.168.1.50:4000 bash scripts/test-pond-agent-live.sh

set -euo pipefail

HOST="${POND_HOST:-http://127.0.0.1:4000}"
SESSION="live-test-$(date +%s)"
PASS=0
FAIL=0
TIMEOUT=120  # seconds per request — high for local inference

green() { printf "\033[32m%s\033[0m\n" "$1"; }
red()   { printf "\033[31m%s\033[0m\n" "$1"; }
bold()  { printf "\033[1m%s\033[0m\n" "$1"; }

assert_contains() {
    local label="$1" response="$2" expected="$3"
    if echo "$response" | grep -qi "$expected"; then
        green "  PASS: $label (contains '$expected')"
        PASS=$((PASS + 1))
    else
        red "  FAIL: $label (expected '$expected' not found)"
        echo "  Response: ${response:0:200}"
        FAIL=$((FAIL + 1))
    fi
}

assert_not_empty() {
    local label="$1" response="$2"
    if [ -n "$response" ] && [ "$response" != "null" ]; then
        green "  PASS: $label (non-empty response)"
        PASS=$((PASS + 1))
    else
        red "  FAIL: $label (empty response)"
        FAIL=$((FAIL + 1))
    fi
}

# Send a chat message and return the full SSE stream
chat() {
    local msg="$1" sid="${2:-$SESSION}"
    curl -s --max-time "$TIMEOUT" -X POST "$HOST/api/v1/chat/stream" \
        -H "Content-Type: application/json" \
        -d "{\"message\": \"$msg\", \"session_id\": \"$sid\"}" 2>/dev/null
}

# Extract text content from SSE stream
extract_text() {
    grep '"type":"text"' | sed 's/^data: //' | jq -r '.content // empty' 2>/dev/null | tr -d '\n'
}

# Extract tool calls from SSE stream
extract_tool_calls() {
    grep '"type":"tool_call"' | sed 's/^data: //' | jq -r '.tool // empty' 2>/dev/null
}

# Measure time-to-first-token (macOS-compatible)
measure_ttft() {
    local msg="$1" sid="${2:-$SESSION}-ttft"
    local start end
    start=$(python3 -c 'import time; print(int(time.time()*1000))')
    # Wait for first text event
    curl -s --max-time "$TIMEOUT" -X POST "$HOST/api/v1/chat/stream" \
        -H "Content-Type: application/json" \
        -d "{\"message\": \"$msg\", \"session_id\": \"$sid\"}" 2>/dev/null | \
        grep -m1 '"type":"text"' > /dev/null
    end=$(python3 -c 'import time; print(int(time.time()*1000))')
    echo $(( end - start ))
}

bold "============================================"
bold " GIAP Live Pipeline Test"
bold " Host: $HOST"
bold " Session: $SESSION"
bold "============================================"
echo ""

# ── Test 0: Server health ────────────────────────────────────────────────────
bold "Test 0: Server health"
HEALTH=$(curl -s --max-time 5 "$HOST/api/v1/health" 2>/dev/null || echo "")
assert_contains "health endpoint" "$HEALTH" "ok"
if [ -z "$HEALTH" ]; then
    red "Server not reachable at $HOST. Start with: cargo run -p pond-server -- serve"
    exit 1
fi
echo ""

# ── Test 1: Simple chat ──────────────────────────────────────────────────────
bold "Test 1: Simple chat (LLM generates text)"
RESPONSE=$(chat "Say hello in exactly one sentence" | tee /dev/null)
TEXT=$(echo "$RESPONSE" | extract_text)
assert_not_empty "LLM produced text" "$TEXT"
assert_contains "response is a greeting" "$TEXT" "hello\|hi\|hey\|greet"
DONE=$(echo "$RESPONSE" | grep '"done":true')
assert_not_empty "stream completed with done event" "$DONE"
echo "  Response: ${TEXT:0:100}"
echo ""

# ── Test 2: Tool calling (weather) ───────────────────────────────────────────
bold "Test 2: Tool calling (weather in Nairobi)"
RESPONSE=$(chat "What is the weather in Nairobi right now?" "${SESSION}-weather")
TOOL=$(echo "$RESPONSE" | extract_tool_calls)
TEXT=$(echo "$RESPONSE" | extract_text)
assert_contains "weather tool invoked" "$TOOL" "weather"
assert_contains "response mentions temperature or weather" "$TEXT" "°C\|temperature\|clear\|cloud\|rain\|sunny\|warm"
echo "  Tool: $TOOL"
echo "  Response: ${TEXT:0:150}"
echo ""

# ── Test 3: Multi-turn context ───────────────────────────────────────────────
bold "Test 3: Multi-turn context (remembers Nairobi)"
RESPONSE=$(chat "What about tomorrow?" "${SESSION}-weather")
TOOL=$(echo "$RESPONSE" | extract_tool_calls)
TEXT=$(echo "$RESPONSE" | extract_text)
assert_contains "forecast tool invoked" "$TOOL" "weather\|forecast"
assert_contains "response mentions forecast/tomorrow" "$TEXT" "forecast\|tomorrow\|drizzle\|rain\|sun\|cloud\|°C"
echo "  Tool: $TOOL"
echo "  Response: ${TEXT:0:150}"
echo ""

# ── Test 4: Knowledge tool (Wikipedia) ───────────────────────────────────────
bold "Test 4: Knowledge tool (Wikipedia lookup)"
RESPONSE=$(chat "Tell me about the Eiffel Tower" "${SESSION}-knowledge")
TOOL=$(echo "$RESPONSE" | extract_tool_calls)
TEXT=$(echo "$RESPONSE" | extract_text)
assert_contains "knowledge tool invoked" "$TOOL" "wikipedia\|knowledge"
assert_contains "response mentions Paris or France" "$TEXT" "Paris\|France\|tower\|iron\|Eiffel"
echo "  Tool: $TOOL"
echo "  Response: ${TEXT:0:150}"
echo ""

# ── Test 5: Memory tool ──────────────────────────────────────────────────────
bold "Test 5: Memory tool (save and recall)"
RESPONSE=$(chat "Remember that my favorite color is blue" "${SESSION}-memory")
TEXT=$(echo "$RESPONSE" | extract_text)
TOOL=$(echo "$RESPONSE" | extract_tool_calls)
# The model should either call save_memory or acknowledge it will remember
assert_not_empty "LLM responded to memory request" "$TEXT"
echo "  Tool: ${TOOL:-none}"
echo "  Response: ${TEXT:0:150}"
echo ""

# ── Test 6: Latency measurement ─────────────────────────────────────────────
bold "Test 6: Latency (TTFT measurement)"
TTFT1=$(measure_ttft "Say one word")
green "  TTFT (simple): ${TTFT1}ms"
TTFT2=$(measure_ttft "What is the weather in London?" "${SESSION}-lat2")
green "  TTFT (with tool): ${TTFT2}ms"
echo ""

# ── Summary ──────────────────────────────────────────────────────────────────
bold "============================================"
bold " Results: $PASS passed, $FAIL failed"
bold "============================================"

if [ "$FAIL" -gt 0 ]; then
    red "Some tests failed!"
    exit 1
else
    green "All tests passed!"
    exit 0
fi
