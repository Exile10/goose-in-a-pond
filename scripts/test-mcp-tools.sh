#!/usr/bin/env bash
# Live MCP tool test suite for GIAP
# Usage: bash scripts/test-mcp-tools.sh [server_url]
#
# Requires: running pond-server, curl, python3
# Produces: test-results.txt with full details per test

set -euo pipefail

URL="${1:-http://127.0.0.1:4000}"
RESULTS_FILE="test-results.txt"
PASS=0
FAIL=0
TOTAL=0

# Colors (terminal only)
G='\033[0;32m' R='\033[0;31m' Y='\033[0;33m' B='\033[1m' N='\033[0m'

# Start fresh results file
cat > "$RESULTS_FILE" <<EOF
GIAP MCP Tool Test Results
Generated: $(date -u '+%Y-%m-%dT%H:%M:%SZ')
Server: $URL
========================================

EOF

log() {
    echo "$1" >> "$RESULTS_FILE"
}

chat() {
    local msg="$1" timeout="${2:-120}"
    curl -s -N -X POST "$URL/api/v1/chat/stream" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer test" \
        -d "{\"message\": \"$msg\"}" \
        --max-time "$timeout" 2>&1 | grep "^data:" | sed 's/^data: //'
}

parse_events() {
    python3 -c "
import sys, json
tools, results, texts, errors = [], [], [], []
tool_inputs = []
done_data = {}
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    try:
        d = json.loads(line)
    except: continue
    t = d.get('type','')
    if t == 'tool_call':
        tools.append(d.get('tool','?'))
        tool_inputs.append(json.dumps(d.get('input')))
    elif t == 'tool_result':
        results.append(d.get('content',''))
    elif t == 'text':
        texts.append(d.get('content',''))
    elif 'done' in d:
        done_data = d
    elif 'error' in d:
        errors.append(d.get('error',''))

text = ''.join(texts)
print('TOOLS:' + '|'.join(tools))
print('TOOL_INPUTS:' + '|'.join(tool_inputs))
print('RESULTS_FULL:' + '|||'.join(results))
print('TEXT:' + text[:500])
print('ERRORS:' + '|'.join(errors))
print('DONE:' + json.dumps(done_data))
"
}

run_test() {
    local name="$1" msg="$2" expect_tool="$3" expect_in_result="$4" timeout="${5:-120}"
    TOTAL=$((TOTAL + 1))

    printf "${B}[%02d] %s${N}\n" "$TOTAL" "$name"
    printf "     Message: %s\n" "$msg"

    log "── Test $TOTAL: $name ──"
    log "Message: $msg"
    log "Expected tool: ${expect_tool:-any}"
    log "Expected in result: ${expect_in_result:-<none>}"
    log ""

    # Mark log position before this test so we can extract per-test ToolCaller output
    local log_lines_before=0
    if [ -n "$SERVER_LOG" ] && [ -f "$SERVER_LOG" ]; then
        log_lines_before=$(wc -l < "$SERVER_LOG")
    fi

    local raw
    raw=$(chat "$msg" "$timeout")
    local parsed
    parsed=$(echo "$raw" | parse_events)

    local tools=$(echo "$parsed" | grep '^TOOLS:' | cut -d: -f2-)
    local tool_inputs=$(echo "$parsed" | grep '^TOOL_INPUTS:' | cut -d: -f2-)
    local results_full=$(echo "$parsed" | grep '^RESULTS_FULL:' | cut -d: -f2-)
    local text=$(echo "$parsed" | grep '^TEXT:' | cut -d: -f2-)
    local errors=$(echo "$parsed" | grep '^ERRORS:' | cut -d: -f2-)

    # Extract ToolCaller activity for THIS test from server log
    if [ -n "$SERVER_LOG" ] && [ -f "$SERVER_LOG" ]; then
        local tc_output
        tc_output=$(tail -n +"$((log_lines_before + 1))" "$SERVER_LOG" 2>/dev/null \
            | grep -E "\[tool-caller\] [╔║╚]|\[tool_caller\] [┌│└]|\[tool_caller\] raw text|\[tool_caller\] extracted|\[tool_caller\] no ToolRequest|\[wikipedia\] resolve_topic|\[schedule\].*ToolCaller|\[memory\] empty" \
            | head -20)
        if [ -n "$tc_output" ]; then
            printf "     ${Y}── ToolCaller ──${N}\n"
            echo "$tc_output" | while IFS= read -r line; do
                printf "     %s\n" "$line"
            done
            log ""
            log "ToolCaller activity:"
            echo "$tc_output" >> "$RESULTS_FILE"
            log ""
        fi
    fi

    log "Tool(s) called: ${tools:-<none>}"
    log "Tool input(s):  ${tool_inputs:-<none>}"
    log ""
    log "Tool result:"
    echo "$results_full" | tr '|||' '\n' | head -20 >> "$RESULTS_FILE"
    log ""
    log "LLM response (first 500 chars):"
    log "$text"
    log ""

    if [ -n "$errors" ]; then
        log "ERRORS: $errors"
    fi

    local passed=true

    # Check tool was called (or not)
    if [ "$expect_tool" = "NONE" ]; then
        if [ -n "$tools" ]; then
            printf "     ${R}FAIL: expected no tool call, got: %s${N}\n" "$tools"
            log "VERDICT: FAIL — unexpected tool call: $tools"
            passed=false
        fi
    elif [ -n "$expect_tool" ]; then
        if echo "$tools" | grep -q "$expect_tool"; then
            printf "     Tool: %s  input: %s\n" "$tools" "$tool_inputs"
        else
            printf "     ${R}FAIL: expected tool '%s', got: '%s'${N}\n" "$expect_tool" "$tools"
            log "VERDICT: FAIL — wrong tool"
            passed=false
        fi
    fi

    # Check result contains expected string
    if [ -n "$expect_in_result" ]; then
        if echo "$results_full$text" | grep -qi "$expect_in_result"; then
            printf "     Result: contains '%s'\n" "$expect_in_result"
        else
            printf "     ${R}FAIL: expected '%s' in result${N}\n" "$expect_in_result"
            printf "     Got result: %.150s\n" "$results_full"
            printf "     Got text: %.150s\n" "$text"
            log "VERDICT: FAIL — missing '$expect_in_result'"
            passed=false
        fi
    fi

    if [ -n "$errors" ]; then
        printf "     ${Y}Error: %s${N}\n" "$errors"
    fi

    if $passed; then
        printf "     ${G}PASS${N}\n"
        log "VERDICT: PASS"
        PASS=$((PASS + 1))
    else
        FAIL=$((FAIL + 1))
    fi
    log ""
    log "────────────────────────────────────────"
    log ""
    echo ""
}

# ── Preflight ────────────────────────────────────────────────────────────────

printf "${B}GIAP MCP Tool Test Suite${N}\n"
printf "Server: %s\n" "$URL"
printf "Results: %s\n\n" "$RESULTS_FILE"

# Health check
if ! curl -s "$URL/api/v1/health" | grep -q "ok"; then
    printf "${R}Server not responding at %s${N}\n" "$URL"
    exit 1
fi

# Check config and write to results
CONFIG=$(curl -s "$URL/api/v1/settings" -H "Authorization: Bearer test" | python3 -c "
import sys, json
d = json.load(sys.stdin)
print(f'chat_model:    {d.get(\"chat_model\",\"?\")}')
print(f'chat_provider: {d.get(\"chat_provider\",\"?\")}')
print(f'tool_model:    {d.get(\"tool_model\",\"none\")}')
" 2>/dev/null || echo "(could not read settings)")

printf "${B}Config:${N}\n"
echo "$CONFIG" | sed 's/^/  /'
echo ""

log "Config:"
echo "$CONFIG" >> "$RESULTS_FILE"
log ""
log "========================================"
log ""

# Find server log early so per-test ToolCaller output works
SERVER_LOG=""
for f in /tmp/giap-test-run.log /tmp/giap-live-test2.log /tmp/giap-live-test.log /tmp/giap-test-server2.log; do
    if [ -f "$f" ]; then SERVER_LOG="$f"; break; fi
done
if [ -n "$SERVER_LOG" ]; then
    printf "Server log: %s\n\n" "$SERVER_LOG"
fi

# ── Tests ────────────────────────────────────────────────────────────────────

printf "${B}── Tool Calling Tests ──${N}\n\n"

run_test \
    "Wikipedia: known person" \
    "who is John Cena?" \
    "giap-knowledge__get_wikipedia_article" \
    "Cena"

run_test \
    "Wikipedia: place" \
    "tell me about Nairobi" \
    "giap-knowledge__get_wikipedia_article" \
    "Kenya"

run_test \
    "Weather" \
    "what is the weather right now?" \
    "giap-weather__get_current_weather" \
    ""

run_test \
    "System info" \
    "how much memory does my system have?" \
    "giap-system__get_system_info" \
    "GB"

run_test \
    "Current time" \
    "what time is it?" \
    "" \
    "202"

run_test \
    "Schedule create" \
    "schedule a weather check every morning at 8am" \
    "giap-schedule__create_schedule" \
    ""

run_test \
    "Schedule list" \
    "list my schedules" \
    "giap-schedule__list_schedules" \
    ""

run_test \
    "Memory save" \
    "remember that my favorite food is ugali" \
    "giap-memory__save_memory" \
    "saved"

printf "${B}── Edge Cases ──${N}\n\n"

run_test \
    "Greeting: no tool expected" \
    "hello, good morning!" \
    "NONE" \
    "" \
    60

run_test \
    "Factual: capital of France" \
    "what is the capital of France?" \
    "giap-knowledge__get_wikipedia_article" \
    "Paris"

run_test \
    "Ambiguous: could be weather or general" \
    "is it cold outside?" \
    "" \
    ""

run_test \
    "Multi-intent (model picks one)" \
    "what is the weather and what time is it?" \
    "" \
    ""

# ── Summary ──────────────────────────────────────────────────────────────────

echo ""
printf "${B}═══════════════════════════════════════${N}\n"
printf "${B}Results: %d/%d passed${N}" "$PASS" "$TOTAL"
if [ "$FAIL" -gt 0 ]; then
    printf " ${R}(%d failed)${N}" "$FAIL"
fi
echo ""
printf "${B}═══════════════════════════════════════${N}\n"

log "========================================"
log "SUMMARY: $PASS/$TOTAL passed ($FAIL failed)"
log "========================================"

# ── Server logs: who resolved params? ────────────────────────────────────────

echo ""
printf "${B}── Param Resolution Log (who did the work?) ──${N}\n"

# Find the server log
SERVER_LOG=""
for f in /tmp/giap-test-run.log /tmp/giap-live-test2.log /tmp/giap-live-test.log /tmp/giap-test-server2.log; do
    if [ -f "$f" ]; then SERVER_LOG="$f"; break; fi
done

if [ -n "$SERVER_LOG" ]; then
    # ── ToolCaller Request/Response detail ──
    log ""
    log "── ToolCaller Request/Response Detail ──"
    log ""
    echo ""
    printf "${B}── ToolCaller Request/Response Detail ──${N}\n"
    grep -E "\[tool-caller\] [╔║╚]|\[tool_caller\] [┌│└]" "$SERVER_LOG" 2>/dev/null | tail -60 | while IFS= read -r line; do
        echo "  $line"
        echo "  $line" >> "$RESULTS_FILE"
    done

    # ── Raw model output ──
    log ""
    log "── ToolCaller Raw Model Output ──"
    log ""
    echo ""
    printf "${B}── ToolCaller Raw Model Output ──${N}\n"
    grep -E "\[tool_caller\] raw text" "$SERVER_LOG" 2>/dev/null | tail -10 | while IFS= read -r line; do
        echo "  $line"
        echo "  $line" >> "$RESULTS_FILE"
    done

    # ── Param resolution chain ──
    log ""
    log "── Param Resolution Chain (who resolved each tool call?) ──"
    log ""
    echo ""
    printf "${B}── Param Resolution Chain ──${N}\n"
    grep -E "\[tool-caller\] ║ status|\[wikipedia\] resolve_topic|\[schedule\].*ToolCaller|\[memory\] empty|\[tool_caller\] extracted" "$SERVER_LOG" 2>/dev/null | tail -20 | while IFS= read -r line; do
        echo "  $line"
        echo "  $line" >> "$RESULTS_FILE"
    done
    echo ""
else
    echo "  (no server log found — check /tmp/giap-*.log)"
fi

printf "Full results written to: ${B}%s${N}\n" "$RESULTS_FILE"
echo ""

exit $FAIL
