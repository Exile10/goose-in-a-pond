#!/usr/bin/env python3
"""mlc_toolcall_probe.py — the decisive MLC-LLM test for GIAP.

Question: does an OpenAI-compatible server emit reliable *streaming* tool_calls
under a GIAP-sized tool payload (~50 tools, ~12K tokens) — or does it degrade to
plain text / abort mid-stream the way direct llama.cpp's `--jinja` parser did
("expected peg-native format")?

This isolates that one risk WITHOUT involving GIAP/Goose: if a server fails here,
it can never work behind GooseAdapter. Stdlib only — runs on the Jetson with no
pip install.

Usage:
  python3 mlc_toolcall_probe.py --base http://127.0.0.1:8000 --model <name> --turns 20
  python3 mlc_toolcall_probe.py --base ... --model ... --list-models   # discover the served model id

Exit code 0 iff the streaming-tool_calls success rate meets --threshold (default
0.95 → 19/20), i.e. MLC is safe to promote to the GIAP end-to-end test.
"""
import argparse
import json
import sys
import time
import urllib.error
import urllib.request

# ── A few realistically-shaped GIAP tools, then synthetic padding to hit the
#    ~12K-token tool budget that GIAP's 57 giap-* tools actually send per turn. ──
REAL_TOOLS = [
    {
        "type": "function",
        "function": {
            "name": "get_current_weather",
            "description": "Get the current weather conditions for a location: "
            "temperature, humidity, wind, and a short description. Use this when "
            "the user asks about weather, temperature, or outdoor conditions.",
            "parameters": {
                "type": "object",
                "properties": {
                    "location": {"type": "string", "description": "City name or 'lat,lon'."},
                    "units": {"type": "string", "enum": ["metric", "imperial"], "description": "Temperature units."},
                },
                "required": ["location"],
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": "get_exchange_rate",
            "description": "Get the current foreign-exchange rate between two "
            "currencies. Use for questions like 'how much is X in Y' or rate "
            "comparisons. Informational only, not financial advice.",
            "parameters": {
                "type": "object",
                "properties": {
                    "from": {"type": "string", "description": "ISO 4217 base currency, e.g. USD."},
                    "to": {"type": "string", "description": "ISO 4217 target currency, e.g. KES."},
                },
                "required": ["from", "to"],
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": "set_device_state",
            "description": "Actuate a smart-home device: power on/off, set "
            "brightness 0-100, target temperature in Celsius, or lock/unlock.",
            "parameters": {
                "type": "object",
                "properties": {
                    "device_id": {"type": "string", "description": "Registered device id."},
                    "power": {"type": "boolean"},
                    "brightness": {"type": "integer", "description": "0-100"},
                    "target_temp": {"type": "number", "description": "Celsius"},
                },
                "required": ["device_id"],
            },
        },
    },
]

# The prompt is engineered so the RIGHT behaviour is a single get_current_weather
# call — a plain-text answer is a soft-fail (model declined), a stream error/abort
# is the hard failure we are hunting.
TRIGGER_MESSAGES = [
    {
        "role": "system",
        "content": "You are a home assistant. When a user asks something a tool "
        "can answer accurately, call the tool. Do not guess weather values.",
    },
    {"role": "user", "content": "What is the current weather in Paris, France right now?"},
]


def _synthetic_tool(i: int) -> dict:
    """A verbose synthetic tool, sized so ~47 of them + the real ones ≈ 12K tokens."""
    return {
        "type": "function",
        "function": {
            "name": f"query_metric_{i:02d}",
            "description": (
                f"Retrieve the stored value and recent history for internal metric "
                f"number {i}. Use this only when the user explicitly asks about "
                f"metric {i} by name; it accepts an optional time range and an "
                f"aggregation mode, and returns the latest reading plus a series."
            ),
            "parameters": {
                "type": "object",
                "properties": {
                    "since": {"type": "string", "description": "ISO-8601 start timestamp."},
                    "until": {"type": "string", "description": "ISO-8601 end timestamp."},
                    "aggregation": {
                        "type": "string",
                        "enum": ["none", "min", "max", "avg"],
                        "description": "How to aggregate the returned series.",
                    },
                },
                "required": [],
            },
        },
    }


def build_tools(target_tokens: int) -> list:
    tools = list(REAL_TOOLS)
    i = 0
    # ~4 chars per token heuristic for the serialized tool array.
    while len(json.dumps(tools)) < target_tokens * 4:
        tools.append(_synthetic_tool(i))
        i += 1
    return tools


def approx_tokens(obj) -> int:
    return len(json.dumps(obj)) // 4


def stream_chat(base: str, model: str, messages: list, tools, timeout: float):
    """POST a streaming chat completion; accumulate tool_call deltas + text.

    Returns a per-turn result dict. `outcome` is one of:
      tool_call  — streaming tool_calls delta(s) with valid JSON args (PASS)
      text_only  — model streamed plain text, no tool call (soft-fail)
      bad_args   — tool_call seen but accumulated arguments are not valid JSON
      stream_error — HTTP/transport error or the server aborted mid-stream (HARD fail)
    """
    payload = {"model": model, "messages": messages, "stream": True, "temperature": 0.0}
    if tools:
        payload["tools"] = tools
        payload["tool_choice"] = "auto"
    data = json.dumps(payload).encode()
    req = urllib.request.Request(
        base.rstrip("/") + "/v1/chat/completions",
        data=data,
        headers={"Content-Type": "application/json"},
        method="POST",
    )

    calls = {}  # index -> {"name": str, "args": str}
    text_len = 0
    finish = None
    completion_tokens = 0
    t0 = time.time()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            for raw in resp:
                line = raw.decode("utf-8", "replace").strip()
                if not line or not line.startswith("data:"):
                    continue
                body = line[5:].strip()
                if body == "[DONE]":
                    break
                try:
                    chunk = json.loads(body)
                except json.JSONDecodeError:
                    return {"outcome": "stream_error", "detail": f"non-JSON SSE chunk: {body[:120]}",
                            "elapsed": time.time() - t0, "completion_tokens": completion_tokens}
                choices = chunk.get("choices") or []
                if not choices:
                    # some servers stream a usage-only final chunk
                    usage = chunk.get("usage") or {}
                    completion_tokens = usage.get("completion_tokens", completion_tokens)
                    continue
                ch = choices[0]
                delta = ch.get("delta") or {}
                if ch.get("finish_reason"):
                    finish = ch["finish_reason"]
                for tc in delta.get("tool_calls") or []:
                    idx = tc.get("index", 0)
                    slot = calls.setdefault(idx, {"name": "", "args": ""})
                    fn = tc.get("function") or {}
                    if fn.get("name"):
                        slot["name"] = fn["name"]
                    if fn.get("arguments"):
                        slot["args"] += fn["arguments"]
                if delta.get("content"):
                    text_len += len(delta["content"])
                completion_tokens += 1
    except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError, ConnectionError) as e:
        detail = getattr(e, "reason", str(e))
        # surface the server's error body if present (this is where "peg-native"
        # style parser failures would show up)
        body = ""
        try:
            body = e.read().decode("utf-8", "replace")[:200]  # type: ignore[attr-defined]
        except Exception:
            pass
        return {"outcome": "stream_error", "detail": f"{detail} {body}".strip(),
                "elapsed": time.time() - t0, "completion_tokens": completion_tokens}

    elapsed = time.time() - t0
    if calls:
        # validate every accumulated tool call's arguments parse as JSON
        for slot in calls.values():
            args = slot["args"].strip() or "{}"
            try:
                json.loads(args)
            except json.JSONDecodeError:
                return {"outcome": "bad_args", "detail": f"{slot['name']} args not JSON: {args[:120]}",
                        "elapsed": elapsed, "completion_tokens": completion_tokens}
        names = ",".join(s["name"] for s in calls.values())
        return {"outcome": "tool_call", "detail": names, "finish": finish,
                "elapsed": elapsed, "completion_tokens": completion_tokens}
    return {"outcome": "text_only", "detail": f"{text_len} chars, finish={finish}",
            "elapsed": elapsed, "completion_tokens": completion_tokens}


def discover_model(base: str, timeout: float):
    try:
        with urllib.request.urlopen(base.rstrip("/") + "/v1/models", timeout=timeout) as r:
            data = json.load(r)
        return [m["id"] for m in data.get("data", [])]
    except Exception as e:
        print(f"could not list models: {e}", file=sys.stderr)
        return []


def bench_decode(base: str, model: str, timeout: float):
    """Plain (no-tools) generation to reproduce raw decode tok/s."""
    msgs = [{"role": "user", "content": "Count from 1 to 100, comma separated."}]
    r = stream_chat(base, model, msgs, None, timeout)
    tok = r.get("completion_tokens", 0)
    el = r.get("elapsed", 0) or 1e-9
    return tok / el, tok, el


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:8000")
    ap.add_argument("--model", default="")
    ap.add_argument("--turns", type=int, default=20)
    ap.add_argument("--tool-tokens", type=int, default=12000, help="approx token budget for the tool array")
    ap.add_argument("--threshold", type=float, default=0.95, help="min streaming tool_call success rate to PASS")
    ap.add_argument("--timeout", type=float, default=180.0)
    ap.add_argument("--list-models", action="store_true")
    args = ap.parse_args()

    if args.list_models or not args.model:
        ids = discover_model(args.base, args.timeout)
        print("served models:", ids)
        if args.list_models:
            return 0
        if not ids:
            print("no --model given and none discovered; aborting", file=sys.stderr)
            return 2
        args.model = ids[0]
        print(f"using model: {args.model}")

    tools = build_tools(args.tool_tokens)
    print(f"tool payload: {len(tools)} tools, ~{approx_tokens(tools)} tokens\n")

    dps, dtok, del_ = bench_decode(args.base, args.model, args.timeout)
    print(f"raw decode (no tools): {dps:.1f} tok/s  ({dtok} tok in {del_:.1f}s)\n")

    counts = {"tool_call": 0, "text_only": 0, "bad_args": 0, "stream_error": 0}
    for n in range(1, args.turns + 1):
        r = stream_chat(args.base, args.model, TRIGGER_MESSAGES, tools, args.timeout)
        counts[r["outcome"]] += 1
        print(f"  turn {n:2d}: {r['outcome']:<12} {r.get('detail','')[:80]}  ({r.get('elapsed',0):.1f}s)")

    ok = counts["tool_call"]
    rate = ok / args.turns
    print("\n── verdict ──")
    print(f"streaming tool_calls OK:  {ok}/{args.turns}  ({rate*100:.0f}%)")
    print(f"plain text (declined):    {counts['text_only']}")
    print(f"bad JSON args:            {counts['bad_args']}")
    print(f"stream errors/aborts:     {counts['stream_error']}   <- the llama.cpp-style failure")
    verdict = "PASS — promote to the GIAP end-to-end test" if rate >= args.threshold \
        else "FAIL — do NOT wire behind Goose; stay on Ollama"
    print(f"\n{verdict} (threshold {args.threshold*100:.0f}%)")
    return 0 if rate >= args.threshold else 1


if __name__ == "__main__":
    sys.exit(main())
