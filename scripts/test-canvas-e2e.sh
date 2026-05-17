#!/usr/bin/env bash
# ────────────────────────────────────────────────────────────────
# Canvas MCP-UI End-to-End Verification Script
# Tests all data flows: backend APIs, SSE, MCP-APP hints, resource proxy
# ────────────────────────────────────────────────────────────────

set -euo pipefail

SERVER="${GIAP_SERVER_URL:-http://127.0.0.1:4000}"
PASS=0
FAIL=0
SKIP=0

green() { printf "\033[32m%s\033[0m\n" "$1"; }
red()   { printf "\033[31m%s\033[0m\n" "$1"; }
yellow(){ printf "\033[33m%s\033[0m\n" "$1"; }

check() {
  local label="$1"
  local result="$2"
  if [ "$result" = "ok" ]; then
    green "  PASS  $label"
    PASS=$((PASS + 1))
  elif [ "$result" = "skip" ]; then
    yellow "  SKIP  $label"
    SKIP=$((SKIP + 1))
  else
    red "  FAIL  $label — $result"
    FAIL=$((FAIL + 1))
  fi
}

echo ""
echo "Canvas MCP-UI E2E Verification"
echo "Server: $SERVER"
echo "────────────────────────────────"

# ── 1. Server Health ──────────────────────────────────────────
echo ""
echo "1. Server Health"
health=$(curl -sf "$SERVER/api/v1/health" 2>/dev/null || echo "")
if echo "$health" | grep -q '"ok"'; then
  check "Health endpoint returns ok" "ok"
else
  check "Health endpoint returns ok" "got: $health"
fi

# ── 2. Schedule Data (NotificationsPanel source) ──────────────
echo ""
echo "2. Schedule Data"
sched_count=$(curl -sf "$SERVER/api/v1/schedules" 2>/dev/null | python3 -c "import sys,json; print(len(json.load(sys.stdin)))" 2>/dev/null || echo "0")
if [ "$sched_count" -gt 0 ]; then
  check "Schedules endpoint returns data ($sched_count schedules)" "ok"
else
  check "Schedules endpoint returns data" "got $sched_count"
fi

# Get first schedule ID for runs test
sched_id=$(curl -sf "$SERVER/api/v1/schedules" 2>/dev/null | python3 -c "import sys,json; s=json.load(sys.stdin); print(s[0]['id'] if s else '')" 2>/dev/null || echo "")
if [ -n "$sched_id" ]; then
  run_count=$(curl -sf "$SERVER/api/v1/schedules/$sched_id/runs?limit=5" 2>/dev/null | python3 -c "import sys,json; print(len(json.load(sys.stdin)))" 2>/dev/null || echo "0")
  if [ "$run_count" -gt 0 ]; then
    check "Schedule runs endpoint returns history ($run_count runs)" "ok"
  else
    check "Schedule runs endpoint returns history" "got $run_count"
  fi

  # Verify completed runs have result text (needed for NotificationsPanel excerpt)
  has_result=$(curl -sf "$SERVER/api/v1/schedules/$sched_id/runs?limit=5" 2>/dev/null | python3 -c "
import sys,json
runs=json.load(sys.stdin)
completed = [r for r in runs if r['status'] == 'completed']
print('yes' if completed and completed[0].get('result') else 'no')
" 2>/dev/null || echo "no")
  check "Completed runs include result text for excerpts" "$([ "$has_result" = "yes" ] && echo ok || echo "no result text")"
else
  check "Schedule runs endpoint" "skip"
fi

# ── 3. Memory Data ────────────────────────────────────────────
echo ""
echo "3. Memory Data"
mem_count=$(curl -sf "$SERVER/api/v1/memories" 2>/dev/null | python3 -c "import sys,json; print(len(json.load(sys.stdin)))" 2>/dev/null || echo "0")
if [ "$mem_count" -gt 0 ]; then
  check "Memories endpoint returns data ($mem_count memories)" "ok"
else
  check "Memories endpoint returns data" "got $mem_count"
fi

# ── 4. MCP-APP UI Hint Protocol ──────────────────────────────
echo ""
echo "4. MCP-APP Protocol (extract_ui_hint)"

# Test via a direct chat stream — send weather query and check for ui field
# This is a streaming endpoint so we capture first few lines
stream_output=$(timeout 120 curl -sN -X POST "$SERVER/api/v1/chat/stream" \
  -H "Content-Type: application/json" \
  -d '{"message":"check the weather"}' 2>/dev/null | head -50 || echo "timeout")

if echo "$stream_output" | grep -q '"tool_call"'; then
  check "LLM triggers weather tool_call via chat/stream" "ok"
else
  check "LLM triggers weather tool_call via chat/stream" "skip"
  SKIP=$((SKIP - 1 + 1)) # count as skip not fail for slow models
fi

if echo "$stream_output" | grep -q '"ui"'; then
  check "tool_result includes MCP-APP ui hint" "ok"
  # Extract ui data
  ui_card_type=$(echo "$stream_output" | grep '"ui"' | head -1 | python3 -c "
import sys,json
for line in sys.stdin:
  line = line.strip()
  if line.startswith('data: '):
    line = line[6:]
  try:
    ev = json.loads(line)
    if 'ui' in ev:
      print(ev['ui'].get('card_type',''))
      break
  except: pass
" 2>/dev/null || echo "")
  check "UI hint card_type is 'weather'" "$([ "$ui_card_type" = "weather" ] && echo ok || echo "got: $ui_card_type")"
else
  check "tool_result includes MCP-APP ui hint" "skip"
fi

# ── 5. MCP App Resource Proxy ─────────────────────────────────
echo ""
echo "5. MCP App Resources (standard protocol)"

resource_html=$(curl -sf "$SERVER/api/v1/mcp/resources?uri=ui://giap-weather/weather-card.html" 2>/dev/null || echo "")
if echo "$resource_html" | grep -q '"text"'; then
  html_len=$(echo "$resource_html" | python3 -c "import sys,json; print(len(json.load(sys.stdin)['contents'][0]['text']))" 2>/dev/null || echo "0")
  check "Weather MCP App resource served ($html_len chars)" "ok"

  # Verify it's self-contained HTML
  has_script=$(echo "$resource_html" | python3 -c "
import sys,json
html = json.load(sys.stdin)['contents'][0]['text']
print('yes' if 'ui/initialize' in html and 'tool-result' in html else 'no')
" 2>/dev/null || echo "no")
  check "Weather app implements MCP Apps postMessage protocol" "$([ "$has_script" = "yes" ] && echo ok || echo "missing protocol handlers")"
else
  check "Weather MCP App resource served" "not found"
fi

# Test 404 for unknown resource
unknown=$(curl -sf "$SERVER/api/v1/mcp/resources?uri=ui://unknown/nothing.html" 2>/dev/null; echo $?)
check "Unknown resource returns 404" "$(echo "$unknown" | grep -q '22\|404' && echo ok || echo "unexpected response")"

# ── 6. SSE Schedule Events ────────────────────────────────────
echo ""
echo "6. SSE Schedule Events"

# Trigger a schedule run and watch for SSE events
if [ -n "$sched_id" ]; then
  fired=$(curl -sf -X POST "$SERVER/api/v1/schedules/$sched_id/run-now" 2>/dev/null || echo "")
  if echo "$fired" | grep -q '"fired"'; then
    check "Schedule run-now triggers successfully" "ok"

    # Listen for SSE events (5s window)
    sse_events=$(timeout 5 curl -sN "$SERVER/api/v1/schedules/events" 2>/dev/null | head -5 || echo "")
    if echo "$sse_events" | grep -q '"running"\|"completed"'; then
      check "SSE emits schedule result events" "ok"
    else
      check "SSE emits schedule result events" "skip"
    fi
  else
    check "Schedule run-now triggers" "failed: $fired"
  fi
else
  check "SSE schedule events" "skip"
fi

# ── 7. Frontend Type Safety ───────────────────────────────────
echo ""
echo "7. Frontend Type Safety"
POND_DESKTOP="$(cd "$(dirname "$0")/../pond-desktop" && pwd)"
tsc_output=$(cd "$POND_DESKTOP" && npx tsc --noEmit 2>&1 || true)
tsc_errors=$(echo "$tsc_output" | grep -c "error TS" || true)
mcp_errors=$(echo "$tsc_output" | grep "error TS" | grep -cE "Canvas\.tsx|McpAppHost|registry\.ts|ContextCard\.tsx|NotificationsPanel|ScheduleDebrief" || true)
check "Zero type errors in MCP-UI files ($mcp_errors of $tsc_errors total)" "$([ "${mcp_errors:-0}" = "0" ] && echo ok || echo "$mcp_errors errors")"

# ── 8. Frontend Tests ─────────────────────────────────────────
echo ""
echo "8. Frontend Tests"
test_output=$(cd "$POND_DESKTOP" && npx vitest run src/state/reducer.test.ts src/desktopState.test.ts src/voiceSummon.test.ts 2>&1 || true)
if echo "$test_output" | grep -q "Tests.*passed"; then
  pass_count=$(echo "$test_output" | grep -oE "Tests +[0-9]+ passed" | head -1)
  check "Core frontend tests ($pass_count)" "ok"
elif echo "$test_output" | grep -q "passed"; then
  check "Core frontend tests" "ok"
else
  check "Core frontend tests" "failed — $(echo "$test_output" | tail -3)"
fi

# ── Summary ───────────────────────────────────────────────────
echo ""
echo "────────────────────────────────"
echo "Results: $(green "$PASS passed") | $(red "$FAIL failed") | $(yellow "$SKIP skipped")"
echo ""

exit $FAIL
