#!/usr/bin/env python3
"""A/B the tool-selection modes on real hardware.

The whole "catalog with a small working set" design rests on one claim nobody
has measured: that narrowing the model's tool surface from 66 tools to ~17 does
not cost it the tool it actually needed. Published benchmarks say accuracy
collapses well before 66 (Haiku drops under 90% between 10 and 15 tools), and
GIAP's own numbers say 59 tools costs 6,539 prompt tokens and 11.4 s TTFT
against 2,386 and 4.0 s for 17. Both of those argue FOR narrowing. Neither of
them is evidence about *this* pond, *these* models and *this* catalog.

## The criteria, fixed before the run

Three sets, and the third is the one that carries the risk.

* MUST-CALL     — a named tool is the only way to answer.
                  `relevant` must not call FEWER of them than `all`.
* MUST-NOT-CALL — answerable from the prompt or from ordinary knowledge.
                  `relevant` must not call MORE tools than `all`.
* CROSS-GROUP   — the answer needs a tool from a group the opening message does
                  NOT obviously score for. This is where narrowing can lose:
                  not "too many tools" but "the one it needed was not loaded".

A pass on the first two with a failure on the third is the honest failure mode
of this design, and the reason it is measured separately rather than folded into
an overall accuracy number that would hide it.

## Discipline

* One session per question. Selection is sticky per session, so reusing one
  would measure the FIRST question's selection for every later question.
* `goal_check_enabled` held fixed across arms — it roughly doubles inferences
  per turn and would otherwise be a confound larger than the effect.
* Every arm reports TTFT and prompt tokens beside accuracy, because the cost
  side is the reason to narrow and a win there means nothing if row three fails.
* Nothing is retried. A run that needs a second attempt is telling you something.
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
import time
import urllib.error
import urllib.request

# ── The question sets ──────────────────────────────────────────────────────


class Q:
    def __init__(self, prompt: str, expect: str | None, why: str):
        self.prompt = prompt
        # Fully-qualified tool name, or None for "no tool should be called".
        self.expect = expect
        self.why = why


MUST_CALL = [
    Q("What is the weather right now?", "giap-weather__get_current_weather",
      "live data; training weights cannot hold today's weather"),
    Q("What time is it?", "giap-system__get_current_time",
      "clock; <system-context> carries it but the tool is the canonical path"),
    Q("List the devices set up in this home.", "giap-device__list_registered_devices",
      "this household's own registry"),
    Q("What is on my schedule?", "giap-schedule__list_schedules",
      "this pond's own scheduled tasks"),
    Q("What is the current price of bitcoin?", "giap-finance__get_crypto_price",
      "changes by the minute"),
    Q("What are today's top tech stories?", "giap-news__search_news",
      "today, by definition not in weights"),
    Q("Convert 100 US dollars to Kenyan shillings.", "giap-finance__convert_currency",
      "live forex"),
    Q("Tell me about the Rust programming language from Wikipedia.",
      "giap-knowledge__get_wikipedia_article", "named source, dedicated tool"),
    Q("Remember that I prefer tea over coffee.", "giap-memory__save_memory",
      "a durable fact about the user"),
    Q("What do you remember about me?", "giap-memory__recall_memories",
      "reads the household store"),
    Q("How much disk space is left on this machine?", "giap-system__get_system_info",
      "this device's own state"),
]

MUST_NOT_CALL = [
    Q("What is the capital of France?", None, "settled fact, cannot have changed"),
    Q("Look up Kenya's population and capital.", None,
      "settled reference data; get_country_info was removed 2026-09-10 for exactly this"),
    Q("How many centimetres are in a metre?", None, "definition"),
    Q("Write me a haiku about rain.", None, "generation, no lookup"),
    Q("What does the word 'ubiquitous' mean, roughly?", None,
      "ordinary vocabulary; define_word exists but is not required"),
    Q("Say hello.", None, "greeting"),
    Q("What is 17 times 23?", None, "arithmetic a model can do"),
    Q("Summarise what you can help me with.", None, "self-description"),
    Q("Translate 'good morning' into French.", None, "translation from weights"),
    Q("Who wrote Pride and Prejudice?", None, "settled fact"),
    Q("Give me three ideas for a birthday present for a keen gardener.", None,
      "open generation"),
    Q("What is the boiling point of water at sea level?", None, "settled fact"),
    Q("Explain what an API is, briefly.", None, "explanation"),
]

# The risk row. Each opens on a topic whose obvious group is NOT the group that
# holds the answer, so a session narrowed on the opening message has to recover
# — by re-scoring, or by the model reaching for `enable_tool_group`.
CROSS_GROUP = [
    Q("I was reading about the Roman Empire. Anyway, what is the weather?",
      "giap-weather__get_current_weather", "opens on knowledge, needs weather"),
    Q("Tell me a joke. Then tell me what devices are in this house.",
      "giap-device__list_registered_devices", "opens on chat, needs device"),
    Q("What is 2 plus 2? Also, what is on my calendar today?",
      "giap-schedule__list_schedules", "opens on arithmetic, needs schedule"),
    Q("Write a short poem. Then check the price of ethereum.",
      "giap-finance__get_crypto_price", "opens on generation, needs finance"),
    Q("Explain photosynthesis. After that, what did the cameras see recently?",
      "giap-vision__get_recent_camera_events", "opens on science, needs vision"),
    Q("Who was Ada Lovelace? Also please remember my birthday is in June.",
      "giap-memory__save_memory", "opens on knowledge, needs memory"),
    Q("Describe the water cycle, then tell me today's headlines.",
      "giap-news__get_headlines", "opens on science, needs news"),
    Q("What is the tallest mountain? And what time is it in Tokyo?",
      "giap-schedule__world_clock", "opens on geography, needs schedule"),
    Q("Recommend a book. Then look up the barcode 5000112637922.",
      "giap-discovery__lookup_product", "opens on recommendation, needs discovery"),
    Q("Tell me about jazz. Then what is my CPU usage?",
      "giap-system__get_system_info", "opens on music, needs system"),
    Q("What is a black hole? Also convert 100 dollars to euros.",
      "giap-finance__convert_currency", "opens on physics, needs finance"),
    Q("Name three trees. Then set a timer for five minutes.",
      "giap-schedule__set_timer", "opens on nature, needs the new timer tool"),
]

SETS = {"must_call": MUST_CALL, "must_not_call": MUST_NOT_CALL, "cross_group": CROSS_GROUP}


# ── Transport ──────────────────────────────────────────────────────────────


def api(base: str, path: str, payload=None, method=None, timeout=60):
    url = f"{base}/api/v1{path}"
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(url, data=data, method=method or ("POST" if data else "GET"))
    req.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(req, timeout=timeout) as r:
        body = r.read().decode()
    return json.loads(body) if body.strip() else {}


def chat(base: str, message: str, session_id: str, timeout: int):
    """One turn. Returns (text, [tool names called], stats, wall_seconds)."""
    url = f"{base}/api/v1/chat/stream"
    payload = {"message": message, "session_id": session_id}
    req = urllib.request.Request(url, data=json.dumps(payload).encode(), method="POST")
    req.add_header("Content-Type", "application/json")

    text, tools, stats = [], [], {}
    started = time.monotonic()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            for raw in r:
                line = raw.decode("utf-8", "replace").strip()
                if not line.startswith("data:"):
                    continue
                try:
                    ev = json.loads(line[5:].strip())
                except json.JSONDecodeError:
                    continue
                kind = ev.get("type")
                if kind == "text":
                    text.append(ev.get("content", ""))
                elif kind == "tool_call":
                    # The event carries the tool name under one of a couple of
                    # keys depending on the path that emitted it.
                    name = ev.get("tool") or ev.get("name") or ""
                    if name:
                        tools.append(name)
                elif kind == "turn_stats":
                    stats = ev.get("stats", ev)
                elif kind == "error":
                    text.append(f"[ERROR] {ev.get('message', '')}")
    except (urllib.error.URLError, TimeoutError) as e:
        return f"[TRANSPORT] {e}", tools, stats, time.monotonic() - started
    return "".join(text), tools, stats, time.monotonic() - started


# ── The run ────────────────────────────────────────────────────────────────


def set_mode(base: str, mode: str, model: str):
    api(base, "/settings", {"tool_selection_mode": mode, "chat_model": model,
                            "goal_check_enabled": True}, method="PUT")


def run_arm(base: str, mode: str, model: str, timeout: int, only: set[str] | None):
    set_mode(base, mode, model)
    time.sleep(1)
    out = {}
    for set_name, questions in SETS.items():
        if only and set_name not in only:
            continue
        rows = []
        for i, q in enumerate(questions):
            # A FRESH session per question: selection is sticky per session, so
            # sharing one would measure the first question's selection twelve
            # times and call it twelve data points.
            sid = f"ab-{mode}-{model}-{set_name}-{i}-{int(time.time())}"
            text, tools, stats, wall = chat(base, q.prompt, sid, timeout)
            called = q.expect in tools if q.expect else bool(tools)
            rows.append({
                "prompt": q.prompt,
                "expect": q.expect,
                "tools": tools,
                "hit": called,
                "ttft_ms": stats.get("ttft_ms"),
                "prompt_tokens": stats.get("prompt_tokens"),
                "tools_count": stats.get("tools_count"),
                "wall_s": round(wall, 1),
                "text": text[:200],
            })
            mark = "." if (called if q.expect else not called) else "X"
            sys.stderr.write(mark)
            sys.stderr.flush()
        sys.stderr.write(f"  {mode}/{model}/{set_name}\n")
        out[set_name] = rows
    return out


def summarise(rows, set_name):
    if not rows:
        return {}
    if set_name == "must_not_call":
        bad = sum(1 for r in rows if r["tools"])
        rate = bad / len(rows)
        key = "spurious_call_rate"
    else:
        good = sum(1 for r in rows if r["hit"])
        rate = good / len(rows)
        key = "tool_call_rate"
    tt = [r["ttft_ms"] for r in rows if r["ttft_ms"]]
    pt = [r["prompt_tokens"] for r in rows if r["prompt_tokens"]]
    tc = [r["tools_count"] for r in rows if r["tools_count"]]
    return {
        key: round(rate, 3),
        "n": len(rows),
        "ttft_ms_median": round(statistics.median(tt)) if tt else None,
        "prompt_tokens_median": round(statistics.median(pt)) if pt else None,
        "tools_in_prompt": round(statistics.median(tc)) if tc else None,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:8080")
    ap.add_argument("--model", default="gemma-4-E2B-it-Q4_K_M")
    ap.add_argument("--timeout", type=int, default=300)
    ap.add_argument("--only", help="comma-separated: must_call,must_not_call,cross_group")
    ap.add_argument("--out", default="/tmp/tool_selection_ab.json")
    args = ap.parse_args()
    only = set(args.only.split(",")) if args.only else None

    result = {"model": args.model, "arms": {}}
    for mode in ("all", "relevant"):
        result["arms"][mode] = run_arm(args.base, mode, args.model, args.timeout, only)

    print(f"\n{'':<16}{'all':>26}{'relevant':>26}")
    verdicts = []
    for set_name in SETS:
        if only and set_name not in only:
            continue
        a = summarise(result["arms"]["all"].get(set_name, []), set_name)
        b = summarise(result["arms"]["relevant"].get(set_name, []), set_name)
        if not a or not b:
            continue
        metric = "spurious_call_rate" if set_name == "must_not_call" else "tool_call_rate"
        print(f"\n{set_name}")
        print(f"  {metric:<14}{a[metric]:>26}{b[metric]:>26}")
        print(f"  {'ttft_ms':<14}{str(a['ttft_ms_median']):>26}{str(b['ttft_ms_median']):>26}")
        print(f"  {'prompt_tokens':<14}{str(a['prompt_tokens_median']):>26}"
              f"{str(b['prompt_tokens_median']):>26}")
        print(f"  {'tools_offered':<14}{str(a['tools_in_prompt']):>26}"
              f"{str(b['tools_in_prompt']):>26}")

        # The criteria, applied.
        if set_name == "must_not_call":
            ok = b[metric] <= a[metric]
            verdicts.append((set_name, ok, "spurious rate must not rise"))
        else:
            ok = b[metric] >= a[metric]
            verdicts.append((set_name, ok, "tool-call rate must not fall"))

    print("\nverdict")
    for name, ok, rule in verdicts:
        print(f"  {'PASS' if ok else 'FAIL'}  {name:<14} {rule}")
    result["verdicts"] = [{"set": n, "pass": ok, "rule": r} for n, ok, r in verdicts]

    with open(args.out, "w") as f:
        json.dump(result, f, indent=2)
    print(f"\nfull rows: {args.out}")
    # cross_group failing is the honest failure of this design; exit non-zero.
    sys.exit(0 if all(ok for _, ok, _ in verdicts) else 1)


if __name__ == "__main__":
    main()
