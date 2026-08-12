#!/usr/bin/env python3
"""Drive all eight PAI capabilities against a real model and report the numbers.

Run through ``scripts/pai-bench.sh``, which owns the server, the scratch data
directory and the port. This file owns the probes.

# The three verdicts, and why SKIP is not a soft PASS

    PASS   the capability was EXERCISED and behaved
    SKIP   it was not exercised, with the reason
    FAIL   it was exercised and did not behave

The distinction is the entire value of this script. This programme has twelve
recorded incidents of a test that passed while proving nothing, and the shape it
keeps taking is a gate observed refusing correctly and reported as the feature
working. The reviewer in PAI-7 ran perfectly on the Orin for 32 seconds and
yielded nothing; every log line looked right, and only the YIELD said so. So a
probe here reports PASS only when it saw the output the capability exists to
produce, and SKIP counts against the run rather than for it.

# Status before body, always

``expect`` refuses to evaluate a body predicate until the status code matched,
because ``body.get("x") is None`` passes against an error payload -- where every
lookup returns None -- and so reports the opposite of the truth.
"""

import json
import os
import sqlite3
import sys
import time
import urllib.error
import urllib.request

PORT = os.environ.get("PAI_BENCH_PORT", "")
DATA = os.environ.get("PAI_BENCH_DATA", "")
MODEL = os.environ.get("PAI_BENCH_MODEL", "")
SLOW = os.environ.get("PAI_BENCH_SLOW", "0") == "1"
ONLY = [s.strip() for s in os.environ.get("PAI_BENCH_ONLY", "").split(",") if s.strip()]
JSON_OUT = os.environ.get("PAI_BENCH_JSON", "")

BASE = "http://127.0.0.1:%s/api/v1" % PORT
LOG = os.path.join(DATA, "server.out")

RESULTS = []          # (pai, name, verdict, detail)
METRICS = {}          # pai -> {metric: value}
TURNS = []            # every turn's stats, in order


# ── plumbing ────────────────────────────────────────────────────────────────

def record(pai, name, verdict, detail=""):
    RESULTS.append((pai, name, verdict, detail))
    mark = {"PASS": "PASS", "FAIL": "FAIL", "SKIP": "SKIP"}[verdict]
    print("  %-4s PAI-%s  %-52s %s" % (mark, pai, name, detail[:80]))
    return verdict == "PASS"


def expect(pai, name, code, want, body, *predicates):
    """Status first, then body predicates."""
    if code != want:
        return record(pai, name, "FAIL", "HTTP %s (wanted %s): %s" % (code, want, str(body)[:120]))
    for sublabel, ok in predicates:
        if not ok:
            return record(pai, name, "FAIL", "%s -- body: %s" % (sublabel, str(body)[:120]))
    return record(pai, name, "PASS")


def req(method, path, payload=None, timeout=120):
    url = BASE + path
    data = json.dumps(payload).encode() if payload is not None else None
    r = urllib.request.Request(url, data=data, method=method)
    if data:
        r.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            raw = resp.read().decode("utf-8", "replace")
            try:
                return resp.status, json.loads(raw)
            except json.JSONDecodeError:
                return resp.status, raw
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", "replace")
        try:
            return e.code, json.loads(raw)
        except json.JSONDecodeError:
            return e.code, raw
    except Exception as e:                       # noqa: BLE001 -- report, never raise
        return 0, str(e)


def chat(message, session_id, timeout=600):
    """One real turn. Returns (text, stats) where stats is the turn_stats frame.

    Reads the SSE stream rather than a JSON body because the metrics this whole
    script exists for -- ttft_ms, prefill_ms, decode_tok_per_sec -- arrive as a
    `turn_stats` event and are not in the final payload.
    """
    url = BASE + "/chat/stream"
    payload = json.dumps({"message": message, "session_id": session_id}).encode()
    r = urllib.request.Request(url, data=payload, method="POST")
    r.add_header("Content-Type", "application/json")
    text, stats, done = [], None, None
    started = time.time()
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            for line in resp:
                line = line.decode("utf-8", "replace").strip()
                if not line.startswith("data:"):
                    continue
                try:
                    ev = json.loads(line[5:].strip())
                except json.JSONDecodeError:
                    continue
                if ev.get("type") == "text":
                    text.append(ev.get("content", ""))
                elif ev.get("type") == "turn_stats":
                    stats = ev
                elif ev.get("done"):
                    done = ev
    except Exception as e:                       # noqa: BLE001
        return "", {"error": str(e)}
    wall_ms = int((time.time() - started) * 1000)
    stats = stats or {}
    if done and "usage" in done:
        stats.setdefault("prompt_tokens", done["usage"].get("prompt_tokens"))
        stats.setdefault("completion_tokens", done["usage"].get("completion_tokens"))
    stats["wall_ms"] = wall_ms
    stats["session_id"] = session_id
    TURNS.append(stats)
    return "".join(text), stats


def settings(patch):
    code, body = req("PUT", "/settings", patch)
    return code == 200, body


def logtext():
    try:
        with open(LOG, "r", errors="replace") as f:
            return f.read()
    except OSError:
        return ""


def refusal_detail(lt):
    """The first refusal the reviewer logged, as the pond itself described it.

    `main.rs` emits `first=Some(Unreadable { index: 0, message: "..." })` on the
    WARN that fires when a run yields nothing, and each individual refusal at
    DEBUG. Either is a real answer; asserting a cause without one is not.
    """
    for marker in ("first=", "refusal="):
        i = lt.rfind(marker)
        if i != -1:
            return lt[i:i + 300].splitlines()[0].strip()
    return ""


def db():
    path = os.path.join(DATA, "pond_system.db")
    if not os.path.exists(path):
        return None
    return sqlite3.connect(path)


def want(pai):
    return not ONLY or str(pai) in ONLY


# ── PAI-3 / PAI-4: what a turn costs, and whether the prefix is reused ──────

def probe_context_and_compaction():
    """One cold turn then a warm one, in the same session.

    The two capabilities are measured together because they are the same two
    turns: PAI-3 is what the first one COSTS, PAI-4 is whether the second one
    reuses it. Splitting them would double the model time for no extra fact.
    """
    print("\n-- PAI-3 (large context) and PAI-4 (compaction / KV reuse) --")
    _, cold = chat("Name three colours. Answer in under ten words.", "bench-ctx")
    if cold.get("error"):
        record(3, "a real turn completes", "FAIL", cold["error"])
        record(4, "the KV prefix is reused across turns", "SKIP", "no first turn to reuse")
        return

    METRICS.setdefault("3", {}).update({
        "prompt_tokens": cold.get("prompt_tokens"),
        "ttft_ms": cold.get("ttft_ms"),
        "prefill_ms": cold.get("prefill_ms"),
        "prefill_tok_per_sec": cold.get("prefill_tok_per_sec"),
        "decode_tok_per_sec": cold.get("decode_tok_per_sec"),
        "context_limit_tokens": cold.get("context_limit_tokens"),
        "context_pct": cold.get("context_pct"),
    })
    expect(3, "a cold turn reports its real cost", 200, 200, cold,
           ("prompt_tokens present", bool(cold.get("prompt_tokens"))))

    # The preamble share. This is the number that decides what to do next on
    # this hardware, and it is invisible from any unit test: tool schemas were
    # ~88% of the prompt when last measured, so the lever is fewer tools rather
    # than more prompt tricks.
    lt = logtext()
    payload = None
    for line in lt.splitlines():
        if "provider payload size" in line:
            payload = line
    if payload:
        def field(k):
            for part in payload.replace("=", " ").split():
                pass
            import re
            m = re.search(k + r"[=: ]+(\d+)", payload)
            return int(m.group(1)) if m else None
        tools_chars = field("tools_json_chars")
        sys_chars = field("system_chars")
        tools_n = field("tools_count")
        METRICS.setdefault("3", {}).update({
            "tools_count": tools_n,
            "tools_json_chars": tools_chars,
            "system_chars": sys_chars,
        })
        if tools_chars and sys_chars:
            share = 100.0 * tools_chars / (tools_chars + sys_chars)
            METRICS["3"]["tool_schema_share_pct"] = round(share, 1)
            record(3, "tool schemas measured as a share of the preamble", "PASS",
                   "%.0f%% of %d chars, %s tools" % (share, tools_chars + sys_chars, tools_n))
    else:
        record(3, "tool schemas measured as a share of the preamble", "SKIP",
               "no 'provider payload size' line -- adapter debug logging off?")

    # Turn two, same session: the prefix should be reused rather than re-prefilled.
    _, warm = chat("And three more, same rules.", "bench-ctx")
    if warm.get("error"):
        record(4, "the KV prefix is reused across turns", "FAIL", warm["error"])
        return
    # Compare against the COLD turn from the liveness check, not against the
    # previous warm one. `cold` here is already warm -- see model_is_alive.
    really_cold = METRICS.get("3", {}).get("cold_prefill_ms")
    METRICS.setdefault("4", {}).update({
        "warm_ttft_ms": warm.get("ttft_ms"),
        "warm_prefill_ms": warm.get("prefill_ms"),
        "cold_prefill_ms": really_cold,
    })

    lt = logtext()
    plans = [l for l in lt.splitlines() if "prefill plan" in l]
    reuse = [l for l in plans if "ReusePrefix" in l]
    if not plans:
        record(4, "the KV prefix is reused across turns", "SKIP",
               "no 'prefill plan' lines -- not the in-process engine, or debug logging off")
        return
    if reuse:
        w_ms = warm.get("prefill_ms")
        if really_cold and w_ms:
            METRICS["4"]["prefill_speedup"] = round(really_cold / w_ms, 1)
            detail = "%.0fx faster prefill than cold (%s ms -> %s ms)" % (
                really_cold / w_ms, really_cold, w_ms)
        else:
            detail = "prefix reused"
        record(4, "the KV prefix is reused across turns", "PASS", detail)
    else:
        record(4, "the KV prefix is reused across turns", "FAIL",
               "turn 2 re-prefilled; something moved the prefix (last plan: %s)" % plans[-1][-60:])


# ── PAI-5: does it think, and does anybody count it ─────────────────────────

def probe_thinking():
    print("\n-- PAI-5 (reasoning) --")
    ok, _ = settings({"show_thinking": True, "reasoning_effort": "thorough",
                      "thinking_mode": "on"})
    if not ok:
        record(5, "reasoning is counted and reported", "SKIP", "could not enable thinking")
        return
    _, st = chat("A farmer has 17 sheep; all but 9 run away. How many remain? "
                 "Think it through, then answer.", "bench-think")
    if st.get("error"):
        record(5, "reasoning is counted and reported", "FAIL", st["error"])
        return
    rt = st.get("reasoning_tokens")
    # The other half of what thinking costs, and the half that was invisible
    # until 2026-08-12: a turn that thinks, says nothing, and gets steered back
    # by EMPTY_TURN_STEER pays for the whole turn twice -- prefill, tools and
    # all. gemma-4-E2B does this reliably for certain phrasings. Reported
    # separately from `inference_count`, which cannot tell a re-engagement from
    # an ordinary tool round-trip.
    re_eng = st.get("reengagements")
    METRICS.setdefault("5", {}).update({
        "reasoning_tokens": rt,
        "completion_tokens": st.get("completion_tokens"),
        "reengagements": re_eng,
    })
    if re_eng:
        print("    NOTE: this turn went silent %s time(s) and had to be steered back; "
              "the reply cost %s full turns." % (re_eng, re_eng + 1))
    if rt is None:
        # None is not zero, and the difference is the phase's whole point:
        # migration 0039 left the column nullable with no DEFAULT so that
        # "nobody counted" stays distinguishable from "it did not reason".
        record(5, "reasoning is counted and reported", "SKIP",
               "reasoning_tokens is null -- nobody counted, which is not the same as zero")
    elif rt > 0:
        record(5, "reasoning is counted and reported", "PASS",
               "%s reasoning + %s answer tokens" % (rt, st.get("completion_tokens")))
    else:
        record(5, "reasoning is counted and reported", "SKIP",
               "the model emitted no thinking block on this prompt")


# ── PAI-1: can one member's data reach another, or a guest ─────────────────

def probe_boundaries():
    print("\n-- PAI-1 (hard profile boundaries) --")
    # 201, not 200. The first version of this probe wanted 200 and reported a
    # FAIL whose detail line was a perfectly good profile -- which is what
    # asserting a status you did not check looks like from the outside.
    code, liz = req("POST", "/profiles", {"display_name": "Liz", "avatar_emoji": "duck"})
    if code not in (200, 201) or not isinstance(liz, dict):
        record(1, "two members exist to be told apart", "FAIL", "HTTP %s: %s" % (code, liz))
        return
    code2, ada = req("POST", "/profiles", {"display_name": "Ada", "avatar_emoji": "duck"})
    if code2 not in (200, 201):
        record(1, "two members exist to be told apart", "FAIL", "HTTP %s" % code2)
        return
    record(1, "two members exist to be told apart", "PASS")

    # With two members on file, an unidentified speaker is a Guest rather than
    # the household -- reached the way production reaches it, not asserted.
    chat("hello", "bench-guest")
    code, body = req("GET", "/context/sources?session_id=bench-guest")
    expect(1, "a guest sees no personal context", code, 200, body,
           ("empty", isinstance(body, dict) and not body.get("sources")))

    # And a guest may not create one either.
    code, body = req("POST", "/context/sources",
                     {"kind": "camera", "provider": "front-door", "session_id": "bench-guest"})
    if code == 403:
        record(1, "a guest cannot claim a context source", "PASS")
    else:
        record(1, "a guest cannot claim a context source", "FAIL",
               "HTTP %s -- an unidentified speaker took ownership" % code)
    METRICS.setdefault("1", {})["members"] = 2
    return liz.get("id")


# ── PAI-2: does a refusal actually refuse ──────────────────────────────────

def probe_privacy(liz_id):
    print("\n-- PAI-2 (privacy and security guardrails) --")
    # Arm the sender FIRST. With weather disabled the route answers
    # `{"enabled": false}` without ever opening a socket, so the probe was
    # measuring a switched-off feature and calling the gate broken.
    # The route's own keys, from routes.rs: weather_latitude / weather_longitude
    # / weather_location_name. Setting `location_name` left it disabled, so the
    # probe measured a switched-off feature and called the gate broken.
    settings({"weather_enabled": True, "weather_latitude": -0.0917,
              "weather_longitude": 34.7680, "weather_location_name": "Kisumu"})
    ok, _ = settings({"network_mode": "offline"})
    if not ok:
        record(2, "offline mode refuses an outbound call", "SKIP", "could not set network_mode")
    else:
        code, body = req("GET", "/weather")
        text = json.dumps(body) if not isinstance(body, str) else body
        # The refusal must NAME the host and the mode. A bare failure is
        # indistinguishable from the network being down, which is the
        # unactionable-refusal failure invariant 1 exists to prevent.
        low = text.lower()
        if '"enabled": false' in low or "'enabled': false" in low:
            # NOT "weather is switched off". `get_weather` gates on
            # `state.weather_provider`, which main.rs wires at STARTUP from
            # settings -- so `PUT /settings {weather_enabled:true}` cannot arm
            # it in a running process, and the route answers `{"enabled":false}`
            # exactly as it would with the setting off. Worth knowing beyond
            # this script: toggling weather in the UI does nothing until the
            # pond restarts, and nothing says so.
            record(2, "offline mode refuses an outbound call, naming it", "SKIP",
                   "the weather provider is wired at startup, so a runtime toggle "
                   "cannot arm it; start with weather_enabled already set")
            settings({"network_mode": "open"})
            named = None
        else:
            named = ("offline" in low) and ("open-meteo" in low or "host" in low)
        if named is None:
            pass
        elif named:
            record(2, "offline mode refuses an outbound call, naming it", "PASS")
        else:
            record(2, "offline mode refuses an outbound call, naming it", "FAIL",
                   "refusal did not name the mode and host: %s" % text[:100])
        settings({"network_mode": "open"})

    # No secret values in the settings payload. `GET /settings` serialises the
    # whole struct, which is exactly how a secret leaks into a UI.
    code, body = req("GET", "/settings")
    if code != 200 or not isinstance(body, dict):
        record(2, "no secret values in GET /settings", "FAIL", "HTTP %s" % code)
    else:
        leaked = [k for k in body
                  if any(w in k.lower() for w in ("secret", "token", "password", "api_key"))
                  and isinstance(body[k], str) and len(body[k]) > 8]
        if leaked:
            record(2, "no secret values in GET /settings", "FAIL", "looks like values: %s" % leaked)
        else:
            record(2, "no secret values in GET /settings", "PASS")


# ── PAI-8: does anything actually reach the corpus ─────────────────────────

def probe_context_streaming(liz_id):
    print("\n-- PAI-8 (personal context streaming) --")
    if not liz_id:
        record(8, "a member can connect a source", "SKIP", "no member to own it")
        return
    # A session owned by Liz, so the source resolves to her.
    chat("hi", "bench-liz")
    code, _ = req("PUT", "/sessions/bench-liz/user", {"profile_id": liz_id})
    if code != 200:
        record(8, "a member can connect a source", "SKIP", "could not attribute a session")
        return

    code, body = req("POST", "/context/sources",
                     {"kind": "camera", "provider": "bench-door", "session_id": "bench-liz"})
    if not expect(8, "a member can connect a source", code, 200, body,
                  ("owned by the member", isinstance(body, dict) and body.get("profile_id") == liz_id)):
        return

    # The ingest path only runs with the toggle on; off is the shipped default.
    ok, _ = settings({"context_ingest_enabled": True})
    if not ok:
        record(8, "a camera event becomes a context item", "SKIP", "could not enable ingest")
        return

    code, _ = req("POST", "/camera/events",
                  {"camera_id": "bench-door", "event_type": "person", "confidence": 0.9})
    if code not in (200, 201):
        record(8, "a camera event becomes a context item", "SKIP",
               "no camera-event route accepted the push (HTTP %s)" % code)
        return

    # The bus is async: poll rather than assume, and report the wait.
    conn = db()
    if conn is None:
        record(8, "a camera event becomes a context item", "SKIP", "no system database")
        return
    found, waited = 0, 0
    for _ in range(20):
        try:
            found = conn.execute("SELECT COUNT(*) FROM context_items").fetchone()[0]
        except sqlite3.Error:
            found = 0
        if found:
            break
        time.sleep(0.5)
        waited += 1
    METRICS.setdefault("8", {})["items_ingested"] = found
    if found:
        record(8, "a camera event becomes a context item", "PASS",
               "%d item(s) after %.1fs" % (found, waited * 0.5))
    else:
        record(8, "a camera event becomes a context item", "FAIL",
               "the source exists and the event was accepted, but nothing was stored")


# ── PAI-6: will it hand work to a subagent ─────────────────────────────────

def probe_orchestration():
    """Two doors reach the orchestrator, and only one is a tool.

    `delegate` lives in the `giap-orchestrator` extension, which
    `register_giap_extensions` registers only when `ext_orchestrator_enabled`
    was true AT STARTUP. The runner arms it and restarts for exactly that
    reason, so an unregistered extension here is a FAILURE of the arming, not a
    reason to skip.

    The other door is `Orchestrator::spawn`, which `init_orchestrator_deps`
    installs unconditionally and PAI-7's reviewer calls directly. So a pond with
    the extension off still has a working orchestrator -- it simply has no tool
    for the model to reach it with. Worth knowing before concluding from a SKIP
    here that delegation is broken.
    """
    print("\n-- PAI-6 (multi-agent orchestration) --")
    lt = logtext()
    if "giap-orchestrator" not in lt:
        record(6, "the delegate tool is registered", "FAIL",
               "the runner armed ext_orchestrator_enabled and restarted, and the extension "
               "still did not register -- check register_giap_extensions")
        return
    record(6, "the delegate tool is registered", "PASS")

    before = lt.count("child")
    _, st = chat("Use your delegation tool to hand a short research task to a saved "
                 "agent role, then tell me what it said.", "bench-delegate", timeout=900)
    if st.get("error"):
        record(6, "a delegation runs a child agent", "FAIL", st["error"])
        return
    lt = logtext()

    # A child actually ran, rather than the model merely talking about it.
    spawned = ("child_stream_step" in lt or "spawn" in lt.lower()
               or lt.count("child") > before + 2)
    if spawned:
        METRICS.setdefault("6", {})["delegated"] = True
        record(6, "a delegation runs a child agent", "PASS")
    else:
        # Not a defect, and not a pass either. A 2B model choosing not to call a
        # tool is a model fact; reporting it as either would be a lie about the
        # capability.
        METRICS.setdefault("6", {})["delegated"] = False
        record(6, "a delegation runs a child agent", "SKIP",
               "the tool was offered and the model did not call it -- a model decision. "
               "PAI-7's reviewer exercises the same orchestrator without a tool (--slow)")


# ── PAI-7: will it think unprompted ────────────────────────────────────────

def probe_proactive():
    print("\n-- PAI-7 (proactive intelligence) --")
    ok, _ = settings({"proactive_review_enabled": True, "ext_orchestrator_enabled": True})
    if not ok:
        record(7, "a proactive review completes and yields", "SKIP", "could not enable the reviewer")
        return
    if not SLOW:
        # The gate is real and the loop is running; neither is the feature. This
        # programme watched the reviewer run correctly for 32 seconds on the
        # Orin and produce nothing, so "the loop ticked" is reported as SKIP.
        lt = logtext()
        ticking = "proactive reviewer" in lt
        record(7, "a proactive review completes and yields", "SKIP",
               "needs 15 min idle; --slow to run it (loop %s)"
               % ("is ticking" if ticking else "NOT seen -- check the startup line"))
        return

    print("     waiting out the 15-minute idle window (--slow)...")
    deadline = time.time() + 20 * 60
    while time.time() < deadline:
        lt = logtext()
        if "starting a proactive review" in lt:
            break
        time.sleep(20)
    lt = logtext()
    if "starting a proactive review" not in lt:
        record(7, "a proactive review completes and yields", "FAIL",
               "the reviewer never started inside 20 minutes")
        return
    for _ in range(40):
        lt = logtext()
        if "proposal made" in lt or "produced nothing" in lt or "impulse refused" in lt:
            break
        time.sleep(15)
    lt = logtext()
    conn = db()
    made = 0
    if conn:
        try:
            made = conn.execute(
                "SELECT COUNT(*) FROM drafts WHERE origin='proactive'").fetchone()[0]
        except sqlite3.Error:
            made = 0
    METRICS.setdefault("7", {})["proposals"] = made
    if made:
        record(7, "a proactive review completes and yields", "PASS", "%d proposal(s)" % made)
    elif "produced nothing" in lt or "impulse refused" in lt:
        # Report the refusal the pond actually recorded, rather than asserting a
        # cause. The previous message here said "the model did not write the
        # schema it was given", which was true of the run that motivated this
        # probe (`deny_unknown_fields` refusing a stray `type` key, fixed
        # 2026-08-12) and would have been wrong about every other reason -- low
        # confidence, a blank rationale, a suppressed shape, the daily cap. A
        # verdict that names a cause it did not check is how a run reports the
        # opposite of the truth.
        record(7, "a proactive review completes and yields", "FAIL",
               "the review ran and every impulse was refused -- %s"
               % (refusal_detail(lt) or "no refusal detail in the log; raise RUST_LOG"))
    else:
        record(7, "a proactive review completes and yields", "FAIL",
               "the review started and nothing came of it")


# ── report ─────────────────────────────────────────────────────────────────

def summarise():
    print("\n" + "=" * 78)
    print("PAI BENCHMARK -- model: %s" % MODEL)
    print("=" * 78)

    order = ["0", "1", "2", "3", "4", "5", "6", "7", "8"]
    names = {
        "1": "hard profile boundaries", "2": "privacy guardrails",
        "3": "large context", "4": "smart compaction",
        "5": "reasoning", "6": "multi-agent orchestration",
        "7": "proactive", "8": "personal context streaming",
        "0": "THE MODEL ITSELF",
    }
    for pai in order:
        rows = [r for r in RESULTS if str(r[0]) == pai]
        if not rows:
            continue
        verdicts = [r[2] for r in rows]
        # A capability that passed one check and skipped another is NOT a pass.
        # Reporting it as one is how a partly-exercised feature comes to look
        # finished, which is the failure this whole script is shaped against.
        if "FAIL" in verdicts:
            overall = "FAIL"
        elif "PASS" in verdicts and "SKIP" in verdicts:
            overall = "PARTIAL"
        elif "PASS" in verdicts:
            overall = "PASS"
        else:
            overall = "SKIP"
        print("  PAI-%s %-28s %s" % (pai, names[pai], overall))
        for _, name, verdict, detail in rows:
            if verdict != "PASS":
                print("        %-6s %s -- %s" % (verdict, name, detail))

    print("\n  Metrics that decide what to do next:")
    m3 = METRICS.get("3", {})
    if m3.get("prompt_tokens"):
        print("    turn 1 prompt        %s tokens" % m3["prompt_tokens"])
    if m3.get("tool_schema_share_pct") is not None:
        print("    tool schemas         %s%% of the preamble (%s tools)"
              % (m3["tool_schema_share_pct"], m3.get("tools_count")))
    if m3.get("cold_ttft_ms"):
        print("    COLD turn TTFT       %s ms  (prefill %s ms, %s tokens)"
              % (m3["cold_ttft_ms"], m3.get("cold_prefill_ms"), m3.get("cold_prompt_tokens")))
    if m3.get("decode_tok_per_sec"):
        print("    decode               %.1f tok/s" % m3["decode_tok_per_sec"])
    m4 = METRICS.get("4", {})
    if m4.get("warm_ttft_ms") is not None:
        print("    WARM turn TTFT       %s ms  (prefill %s ms)%s"
              % (m4["warm_ttft_ms"], m4.get("warm_prefill_ms"),
                 "  -- %sx faster prefill" % m4["prefill_speedup"]
                 if m4.get("prefill_speedup") else ""))
    if m3.get("context_limit_tokens"):
        print("    context window       %s tokens (%.0f%% used on turn 1)"
              % (m3["context_limit_tokens"], m3.get("context_pct") or 0))
    m5 = METRICS.get("5", {})
    if m5.get("reasoning_tokens"):
        print("    reasoning            %s tokens" % m5["reasoning_tokens"])
    # `is not None` and not truthiness: 0 is the ordinary turn and reporting it
    # is the point -- a blank line here would read as "not measured", which is
    # the one thing this field exists to distinguish.
    if m5.get("reengagements") is not None:
        print("    re-engagements       %s (empty turns re-steered)" % m5["reengagements"])

    n_fail = sum(1 for r in RESULTS if r[2] == "FAIL")
    n_skip = sum(1 for r in RESULTS if r[2] == "SKIP")
    n_pass = sum(1 for r in RESULTS if r[2] == "PASS")
    print("\n  %d passed, %d failed, %d not exercised" % (n_pass, n_fail, n_skip))
    if n_skip:
        print("  NOT EXERCISED is not a pass. Each one above says what it would take.")

    if JSON_OUT:
        with open(JSON_OUT, "w") as f:
            json.dump({"model": MODEL, "metrics": METRICS, "turns": TURNS,
                       "results": [{"pai": p, "check": c, "verdict": v, "detail": d}
                                   for p, c, v, d in RESULTS]}, f, indent=2)
        print("  metrics written to %s" % JSON_OUT)

    return 1 if n_fail else 0


def model_is_alive():
    """One cheap turn before anything else, and a diagnosis if it fails.

    The first run of this script picked a model whose architecture the shipped
    llama.cpp cannot load. Every turn failed, and the report was two
    unrelated-looking FAILs and five SKIPs spread across seven capabilities --
    none of which mentioned the model. The root cause was only in the log dig at
    the very bottom.

    So: establish that the subject can answer at all, and if it cannot, say why
    ONCE, quoting llama.cpp's own line. Everything downstream is then honestly
    reported as not exercised rather than as seven separate defects.
    """
    text, st = chat("Reply with the single word: ready", "bench-alive", timeout=900)
    if not st.get("error") and st.get("prompt_tokens"):
        # THIS is the cold turn -- the model was not loaded and no prefix was
        # cached. The first version of this script measured "turn 1" in a later
        # probe and reported 122 ms TTFT as a cold start, because this turn had
        # already warmed the KV prefix. It turned a ~100x cache win into a
        # meaningless 0.8x. The genuinely first turn is the only cold one there
        # is, so its numbers are the cold numbers.
        METRICS.setdefault("3", {}).update({
            "cold_prompt_tokens": st.get("prompt_tokens"),
            "cold_ttft_ms": st.get("ttft_ms"),
            "cold_prefill_ms": st.get("prefill_ms"),
            "model_load_ms": st.get("model_load_ms"),
        })
        return True

    lt = logtext()
    reason = ""
    for needle in ("unknown model architecture", "failed to load model",
                   "null result from llama cpp", "error loading model"):
        for line in lt.splitlines():
            if needle in line:
                reason = line.strip()[-160:]
                break
        if reason:
            break
    record(0, "the model under test can answer at all", "FAIL",
           reason or st.get("error") or "no tokens and no diagnosable log line")
    print("\n  The model did not load, so nothing below was exercised.")
    print("  Pick another with --model NAME, or check the log:")
    print("    %s" % LOG)
    return False


def main():
    if not PORT or not DATA:
        print("run me through scripts/pai-bench.sh", file=sys.stderr)
        return 2

    # The API is gated behind onboarding; a fresh dir has an EMPTY table.
    conn = db()
    if conn:
        conn.execute("INSERT OR REPLACE INTO onboarding_state (id, current_step) VALUES (1, 'Completed')")
        conn.commit()
    settings({"chat_provider": "local", "chat_model": MODEL})

    # Nothing below is meaningful if the subject cannot answer.
    if not model_is_alive():
        return summarise()

    liz = None
    if want(1):
        liz = probe_boundaries()
    if want(2):
        probe_privacy(liz)
    if want(3) or want(4):
        probe_context_and_compaction()
    if want(5):
        probe_thinking()
    if want(6):
        probe_orchestration()
    if want(8):
        probe_context_streaming(liz)
    if want(7):
        probe_proactive()
    return summarise()


if __name__ == "__main__":
    sys.exit(main())
