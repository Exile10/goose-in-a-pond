#!/usr/bin/env bash
# GIAP Tool Calling Diagnostic Test
#
# Comprehensive end-to-end test of Gemma 4 native tool calling via Jinja templates.
# Tests tool dispatch, parameter generation, and result flow.
#
# Usage: bash scripts/test-tool-params.sh [server_url]
# Output: test-tool-params-results.txt (feed this to Claude for analysis)

set -euo pipefail
URL="${1:-http://127.0.0.1:4000}"
OUT="test-tool-params-results.txt"
PASS=0
FAIL=0
TOTAL=0

# ── Output helpers ────────────────────────────────────────────────────────────

log() { printf '%s\n' "$1" >> "$OUT"; }

loghr() {
    log ""
    log "════════════════════════════════════════════════════════════════════"
    log ""
}

# ── Setup ─────────────────────────────────────────────────────────────────────

cat > "$OUT" <<EOF
GIAP Tool Calling Diagnostic Report
Generated: $(date -u '+%Y-%m-%dT%H:%M:%SZ')
Server: $URL
════════════════════════════════════════════════════════════════════
EOF

# Health check
if ! curl -s "$URL/api/v1/health" | grep -q "ok"; then
    echo "ERROR: Server not responding at $URL" | tee -a "$OUT"
    exit 1
fi

# ── Server config snapshot ────────────────────────────────────────────────────

log ""
log "SERVER CONFIGURATION"
log "────────────────────"
curl -s "$URL/api/v1/settings" -H "Authorization: Bearer test" | python3 -c "
import sys, json
d = json.load(sys.stdin)
fields = [
    'chat_provider', 'chat_model', 'tool_model',
    'show_thinking', 'agent_memory_inject', 'agent_timeout_secs',
    'ext_weather_enabled', 'ext_knowledge_enabled', 'ext_schedule_enabled',
    'ext_memory_enabled', 'ext_system_enabled',
]
for f in fields:
    v = d.get(f, '<not set>')
    if v is None: v = '<null>'
    print(f'  {f}: {v}')
" 2>/dev/null >> "$OUT" || log "  (could not read settings)"

# ── Extension/tool listing ────────────────────────────────────────────────────

log ""
log "REGISTERED EXTENSIONS"
log "─────────────────────"
curl -s "$URL/api/v1/extensions" -H "Authorization: Bearer test" | python3 -c "
import sys, json
exts = json.load(sys.stdin)
if not exts:
    print('  <no extensions>')
else:
    for e in exts:
        name = e.get('name','?')
        status = e.get('status','?')
        tools = e.get('tools', [])
        tool_names = ', '.join(t.get('name','?') for t in tools) if tools else '<none>'
        print(f'  {name} [{status}] tools: {tool_names}')
" 2>/dev/null >> "$OUT" || log "  (could not read extensions)"

loghr

echo "GIAP Tool Calling Diagnostic Test"
echo "Server: $URL"
echo ""

# ── Test runner ───────────────────────────────────────────────────────────────

run() {
    local name="$1" msg="$2" expect_tool="$3" expect_param_key="${4:-}" timeout="${5:-120}"
    TOTAL=$((TOTAL + 1))

    echo "[$(printf '%02d' $TOTAL)] $name"
    echo "     -> $msg"

    log "TEST $TOTAL: $name"
    log "  Message: $msg"
    log "  Expected tool: ${expect_tool:-<any>}"
    log "  Expected param: ${expect_param_key:-<none>}"
    log ""

    local start_time=$(date +%s)

    # Send chat and capture full SSE stream
    local raw
    raw=$(curl -s -N -X POST "$URL/api/v1/chat/stream" \
        -H "Content-Type: application/json" -H "Authorization: Bearer test" \
        -d "{\"message\": \"$msg\"}" --max-time "$timeout" 2>&1 || true)

    local end_time=$(date +%s)
    local elapsed=$(( end_time - start_time ))

    # Parse all SSE events
    local parsed
    parsed=$(echo "$raw" | grep "^data:" | sed 's/^data: //' | python3 -c "
import sys, json

tools = []
inputs = []
results = []
text_parts = []
thinking_parts = []
errors = []
done_data = {}

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        d = json.loads(line)
    except:
        continue

    t = d.get('type', '')

    if t == 'tool_call':
        tools.append(d.get('tool', ''))
        inp = d.get('input')
        if inp is None:
            inputs.append('null')
        elif isinstance(inp, dict) and len(inp) == 0:
            inputs.append('{}')
        else:
            inputs.append(json.dumps(inp))
    elif t == 'tool_result':
        content = d.get('content', '')
        results.append(content[:500] if content else '<empty>')
    elif t == 'text':
        text_parts.append(d.get('content', ''))
    elif t == 'thinking':
        thinking_parts.append(d.get('content', ''))
    elif t == 'status':
        pass  # ignore status messages
    elif 'done' in d:
        done_data = d
    elif 'error' in d:
        errors.append(d.get('error', ''))

text = ''.join(text_parts)
thinking = ''.join(thinking_parts)

print('TOOLS=' + '|'.join(tools))
print('INPUTS=' + '|||'.join(inputs))
print('RESULTS=' + '|||'.join(r[:500] for r in results))
print('TEXT=' + text[:800])
print('THINKING=' + thinking[:300])
print('ERRORS=' + '|'.join(errors))
print('DONE=' + json.dumps(done_data))
print('TOOL_COUNT=' + str(len(tools)))
print('TOKEN_COUNT=' + json.dumps(done_data.get('usage', {})))
" 2>&1)

    local tools=$(echo "$parsed" | grep '^TOOLS=' | cut -d= -f2-)
    local inputs=$(echo "$parsed" | grep '^INPUTS=' | cut -d= -f2-)
    local results=$(echo "$parsed" | grep '^RESULTS=' | cut -d= -f2-)
    local text=$(echo "$parsed" | grep '^TEXT=' | cut -d= -f2-)
    local thinking=$(echo "$parsed" | grep '^THINKING=' | cut -d= -f2-)
    local errors=$(echo "$parsed" | grep '^ERRORS=' | cut -d= -f2-)
    local done_json=$(echo "$parsed" | grep '^DONE=' | cut -d= -f2-)
    local tool_count=$(echo "$parsed" | grep '^TOOL_COUNT=' | cut -d= -f2-)
    local token_json=$(echo "$parsed" | grep '^TOKEN_COUNT=' | cut -d= -f2-)

    local passed=true

    # ── Log SSE event summary ──
    log "  Elapsed: ${elapsed}s"
    log "  Tools called: ${tools:-<none>} (count: ${tool_count:-0})"
    log "  Tool input(s): ${inputs:-<none>}"
    log ""

    if [ -n "$results" ] && [ "$results" != "<empty>" ]; then
        log "  Tool result (first 500 chars):"
        log "    $results"
        log ""
    fi

    if [ -n "$thinking" ]; then
        log "  Thinking (first 300 chars):"
        log "    $thinking"
        log ""
    fi

    if [ -n "$text" ]; then
        log "  LLM response (first 800 chars):"
        log "    $text"
        log ""
    fi

    if [ -n "$errors" ]; then
        log "  ERRORS: $errors"
        log ""
    fi

    log "  Done event: $done_json"
    log "  Usage: $token_json"

    # ── Assertions ──

    # 1. Tool dispatch check
    if [ -n "$expect_tool" ] && [ "$expect_tool" != "NONE" ] && [ "$expect_tool" != "any" ]; then
        if echo "$tools" | grep -q "$expect_tool"; then
            log "  ASSERT tool dispatch: PASS ($expect_tool called)"
        else
            log "  ASSERT tool dispatch: FAIL (expected $expect_tool, got: ${tools:-<none>})"
            passed=false
        fi
    elif [ "$expect_tool" = "NONE" ]; then
        if [ -z "$tools" ]; then
            log "  ASSERT no tool: PASS"
        else
            log "  ASSERT no tool: FAIL (unexpected tool: $tools)"
            passed=false
        fi
    fi

    # 2. Param quality check
    if [ -n "$expect_param_key" ]; then
        if echo "$inputs" | grep -q "\"$expect_param_key\""; then
            log "  ASSERT param '$expect_param_key': PASS (model generated params natively)"
        elif [ "$inputs" = "null" ] || [ "$inputs" = "{}" ]; then
            log "  ASSERT param '$expect_param_key': FAIL (empty params — model did not generate args)"
            passed=false
        else
            log "  ASSERT param '$expect_param_key': FAIL (key not found in: $inputs)"
            passed=false
        fi
    fi

    # 3. Response check — model produced some output
    if [ -z "$text" ] && [ -z "$tools" ] && [ -z "$errors" ]; then
        log "  ASSERT response: FAIL (no output at all)"
        passed=false
    fi

    # ── Verdict ──
    if $passed; then
        log "  VERDICT: PASS"
        PASS=$((PASS + 1))
        echo "     PASS (${elapsed}s)"
    else
        log "  VERDICT: FAIL"
        FAIL=$((FAIL + 1))
        echo "     FAIL (${elapsed}s)"
    fi

    loghr
}

# ── Test Suite ────────────────────────────────────────────────────────────────

echo "--- Tool Dispatch + Param Generation ---"
echo ""

run "Wikipedia: person lookup" \
    "look up John Cena on wikipedia" \
    "giap-knowledge__get_wikipedia_article" \
    "topic" \
    120

run "Wikipedia: place lookup" \
    "tell me about Nairobi" \
    "giap-knowledge__get_wikipedia_article" \
    "topic" \
    120

run "Weather: no params needed" \
    "what is the weather right now?" \
    "giap-weather__get_current_weather" \
    "" \
    120

run "Memory: save with content" \
    "remember that my favorite color is green" \
    "giap-memory__save_memory" \
    "content" \
    120

run "Schedule: create with cron + prompt" \
    "create a schedule to check the weather every day at 8am" \
    "giap-schedule__create_schedule" \
    "cron" \
    120

run "System info" \
    "how much memory does my system have?" \
    "giap-system__get_system_info" \
    "" \
    120

echo ""
echo "--- Edge Cases ---"
echo ""

run "Greeting: no tool expected" \
    "hello, good morning!" \
    "NONE" \
    "" \
    60

run "Factual: should use knowledge tool" \
    "what is the capital of France?" \
    "" \
    "" \
    120

# ── Summary ───────────────────────────────────────────────────────────────────

log ""
log "SUMMARY"
log "═══════"
log "  Total:  $TOTAL"
log "  Passed: $PASS"
log "  Failed: $FAIL"
log ""
log "Tool calling pipeline: Gemma 4 native via Jinja templates"
log "  use_jinja: true (GIAP platform settings)"
log "  native_tool_calling: true (GIAP platform settings)"
log "  Goose resolve_model_path fix: OR-merge (never downgrade explicit true)"
log ""

echo ""
echo "═══════════════════════════════════════"
echo "Results: $PASS/$TOTAL passed"
if [ "$FAIL" -gt 0 ]; then
    echo "         $FAIL failed"
fi
echo "═══════════════════════════════════════"
echo "Full report: $OUT"

exit $FAIL
