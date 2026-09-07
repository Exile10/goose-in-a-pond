#!/usr/bin/env python3
"""
bakeoff_turns.py — drive one inference candidate through the bake-off workloads.

Two transports, one metric definition. That is the whole point: a decode rate
measured through GIAP and one measured against a raw HTTP server are only
comparable if "time to first token" means the same thing in both, so both are
timed client-side from just before the request to the first chunk carrying
actual content (a role-only opening chunk is not content).

  --mode giap    POST /api/v1/chat/stream, parse GIAP's SSE frames. Engine-side
                 numbers come from the `turn_stats` frame and are reported
                 alongside the client-side ones, never instead of them.
  --mode openai  POST /v1/chat/completions. Replays a payload captured from the
                 shim (GIAP_CAPTURE_PAYLOAD) so the engine answers the prompt
                 GIAP actually sends -- the tool array, its order, and the
                 enforced system prompt, none of which survive in goose's
                 sessions.db. llama.cpp's `timings` block is read when present.

Stdlib only; the device has Python 3.10 and no venv worth depending on.

Exit code 0 unless a workload could not run at all. Reliability failures are
DATA, not errors: report.py applies the gates.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import statistics
import sys
import time
import urllib.error
import urllib.request

# ── the question sets, kept in step with scripts/tool_selection_ab.py ─────────
# Trimmed to the rows that do not need a live network dependency to be gradeable.
MUST_CALL = [
    ("What is the weather right now?", "giap-weather__get_current_weather"),
    ("What time is it?", "giap-system__get_current_time"),
    ("List the devices set up in this home.", "giap-device__list_registered_devices"),
    ("What is on my schedule?", "giap-schedule__list_schedules"),
    ("What is the current price of bitcoin?", "giap-finance__get_crypto_price"),
    ("What are today's top tech stories?", "giap-news__get_top_stories"),
    ("Look up Kenya's population and capital.", "giap-discovery__get_country_info"),
    ("What is the exchange rate from USD to KES?", "giap-finance__get_exchange_rate"),
    ("Tell me about the Rust programming language from Wikipedia.",
     "giap-knowledge__get_wikipedia_article"),
    ("Remember that I prefer tea over coffee.", "giap-memory__save_memory"),
    ("What do you remember about me?", "giap-memory__recall_memories"),
    ("How much disk space is left on this machine?", "giap-system__get_system_info"),
]

MUST_NOT_CALL = [
    "What is the capital of France?",
    "How many centimetres are in a metre?",
    "Write me a haiku about rain.",
    "Say hello.",
    "What is 17 times 23?",
    "Summarise what you can help me with.",
    "Translate 'good morning' into French.",
    "Who wrote Pride and Prejudice?",
    "What is the boiling point of water at sea level?",
    "Explain what an API is, briefly.",
]

# Ten factual prompts with a stable one-word-ish answer. Not a benchmark of the
# model -- a canary that a memory-saving configuration (KV quant, iSWA, a
# drafter, a smaller quant) did not quietly change what the model says.
CANARY = [
    ("What is the capital of France?", "paris"),
    ("What is 17 times 23?", "391"),
    ("How many centimetres are in a metre?", "100"),
    ("Who wrote Pride and Prejudice?", "austen"),
    ("What is the boiling point of water at sea level in Celsius?", "100"),
    ("What is the chemical symbol for gold?", "au"),
    ("How many continents are there?", "seven"),
    ("What planet is known as the red planet?", "mars"),
    ("Translate 'good morning' into French.", "bonjour"),
    ("What is the largest ocean on Earth?", "pacific"),
]

SENTENCE_END = re.compile(r"[.!?]")


# ── timing container ─────────────────────────────────────────────────────────
class Turn:
    """One request's client-side timeline, plus whatever the server volunteered."""

    def __init__(self) -> None:
        self.outcome = "ok"
        self.detail = ""
        self.t0 = 0.0
        self.t_first: float | None = None      # first chunk carrying content
        self.t_first_sentence: float | None = None
        self.t_last: float | None = None
        self.text = ""
        self.tools: list[str] = []
        self.chunks = 0
        self.usage: dict = {}
        self.timings: dict = {}                # llama.cpp server only
        self.engine_stats: dict = {}           # GIAP turn_stats only

    def mark_content(self, piece: str = "") -> None:
        now = time.monotonic()
        if self.t_first is None:
            self.t_first = now
        self.t_last = now
        if piece:
            self.text += piece
            if self.t_first_sentence is None and SENTENCE_END.search(self.text):
                self.t_first_sentence = now

    def as_dict(self) -> dict:
        ttft = None if self.t_first is None else (self.t_first - self.t0) * 1000
        ttfs = None if self.t_first_sentence is None else (self.t_first_sentence - self.t0) * 1000
        wall = ((self.t_last or time.monotonic()) - self.t0) * 1000
        # Decode is measured from first content to last, so prefill is excluded.
        # completion_tokens from `usage` when the server sends it; chunk count is
        # the fallback and is labelled as such, because a chunk is not a token.
        decode_ms = None
        if self.t_first is not None and self.t_last is not None:
            decode_ms = (self.t_last - self.t_first) * 1000
        comp = self.usage.get("completion_tokens")
        token_source = "usage"
        if comp is None:
            comp, token_source = self.chunks, "chunk_count"
        rate = None
        if decode_ms and decode_ms > 0 and comp:
            rate = comp * 1000.0 / decode_ms
        return {
            "outcome": self.outcome,
            "detail": self.detail,
            "ttft_ms": ttft,
            "ttfs_ms": ttfs,
            "wall_ms": wall,
            "decode_ms": decode_ms,
            "decode_tok_per_sec": rate,
            "completion_tokens": comp,
            "completion_token_source": token_source,
            "prompt_tokens": self.usage.get("prompt_tokens"),
            "cached_prompt_tokens": (self.usage.get("prompt_tokens_details") or {}).get("cached_tokens"),
            "text_chars": len(self.text),
            "tools": self.tools,
            "timings": self.timings or None,
            "engine_stats": self.engine_stats or None,
        }


# ── OpenAI transport ─────────────────────────────────────────────────────────
def stream_openai(base: str, body: dict, timeout: float) -> Turn:
    t = Turn()
    calls: dict[int, dict] = {}
    req = urllib.request.Request(
        base.rstrip("/") + "/v1/chat/completions",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    t.t0 = time.monotonic()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            for raw in resp:
                line = raw.decode("utf-8", "replace").strip()
                if not line.startswith("data:"):
                    continue
                payload = line[5:].strip()
                if payload == "[DONE]":
                    break
                try:
                    chunk = json.loads(payload)
                except json.JSONDecodeError:
                    t.outcome, t.detail = "stream_error", f"non-JSON SSE chunk: {payload[:120]}"
                    return t
                if chunk.get("usage"):
                    t.usage = chunk["usage"]
                if chunk.get("timings"):
                    t.timings = chunk["timings"]
                choices = chunk.get("choices") or []
                if not choices:
                    continue
                delta = choices[0].get("delta") or {}
                # A role-only opening chunk is not content and must not start the
                # clock: counting it would flatter every server that sends one.
                if delta.get("content"):
                    t.mark_content(delta["content"])
                    t.chunks += 1
                if delta.get("reasoning_content") or delta.get("reasoning"):
                    t.mark_content()
                    t.chunks += 1
                for tc in delta.get("tool_calls") or []:
                    t.mark_content()
                    slot = calls.setdefault(tc.get("index", 0), {"name": "", "args": ""})
                    fn = tc.get("function") or {}
                    if fn.get("name"):
                        slot["name"] = fn["name"]
                    if fn.get("arguments"):
                        slot["args"] += fn["arguments"]
    except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError, ConnectionError) as e:
        body_text = ""
        try:
            body_text = e.read().decode("utf-8", "replace")[:300]  # type: ignore[attr-defined]
        except Exception:
            pass
        t.outcome = "stream_error"
        t.detail = f"{getattr(e, 'reason', e)} {body_text}".strip()
        return t

    for slot in calls.values():
        try:
            json.loads(slot["args"].strip() or "{}")
        except json.JSONDecodeError:
            t.outcome = "bad_args"
            t.detail = f"{slot['name']} args not JSON: {slot['args'][:120]}"
        t.tools.append(slot["name"])
    if t.outcome == "ok" and not t.tools and not t.text:
        t.outcome, t.detail = "empty", "no content and no tool call"
    return t


# ── GIAP transport ───────────────────────────────────────────────────────────
def stream_giap(base: str, message: str, session_id: str, timeout: float) -> Turn:
    t = Turn()
    req = urllib.request.Request(
        base.rstrip("/") + "/api/v1/chat/stream",
        data=json.dumps({"message": message, "session_id": session_id}).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    t.t0 = time.monotonic()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            for raw in resp:
                line = raw.decode("utf-8", "replace").strip()
                if not line.startswith("data:"):
                    continue
                try:
                    ev = json.loads(line[5:].strip())
                except json.JSONDecodeError:
                    continue
                kind = ev.get("type")
                if kind == "text":
                    t.mark_content(ev.get("content", ""))
                    t.chunks += 1
                elif kind == "tool_call":
                    t.mark_content()
                    name = ev.get("tool") or ev.get("name") or ""
                    if name:
                        t.tools.append(name)
                elif kind == "turn_stats":
                    t.engine_stats = ev.get("stats", ev)
                elif kind == "error":
                    t.outcome = "stream_error"
                    t.detail = ev.get("message", "")
    except (urllib.error.URLError, TimeoutError, ConnectionError) as e:
        t.outcome, t.detail = "stream_error", str(e)
        return t

    st = t.engine_stats or {}
    # Prefer the engine's own token counts; they are exact where the client's
    # chunk count is a proxy.
    if st.get("completion_tokens") is not None:
        t.usage["completion_tokens"] = st["completion_tokens"]
    if st.get("prompt_tokens") is not None:
        t.usage["prompt_tokens"] = st["prompt_tokens"]
    if t.outcome == "ok" and not t.tools and not t.text:
        t.outcome, t.detail = "empty", "no content and no tool call"
    return t


# ── payload replay ───────────────────────────────────────────────────────────
def load_payload(path: str) -> dict:
    with open(path) as f:
        body = json.load(f)
    body["stream"] = True
    body.setdefault("stream_options", {})["include_usage"] = True
    body["temperature"] = 0.0
    return body


def with_user_message(body: dict, message: str) -> dict:
    """Swap the trailing user turn, holding the tool surface and system prompt fixed.

    Varying the ask while the tools stay byte-identical is what makes a
    reliability score attributable to the engine rather than to the prompt.
    """
    out = json.loads(json.dumps(body))
    msgs = out.get("messages") or []
    for i in range(len(msgs) - 1, -1, -1):
        if msgs[i].get("role") == "user":
            msgs[i] = {"role": "user", "content": message}
            break
    else:
        msgs.append({"role": "user", "content": message})
    out["messages"] = msgs
    return out


# ── workloads ────────────────────────────────────────────────────────────────
def summarise(values: list[float | None]) -> dict | None:
    vals = [v for v in values if v is not None]
    if not vals:
        return None
    return {
        "n": len(vals),
        "median": round(statistics.median(vals), 2),
        "min": round(min(vals), 2),
        "max": round(max(vals), 2),
        # A spread this wide means the runs were not in the same state.
        "spread_pct": round((max(vals) - min(vals)) * 100.0 / statistics.median(vals), 1)
        if statistics.median(vals) else None,
    }


class Driver:
    def __init__(self, args) -> None:
        self.a = args
        self.payloads: dict[str, dict] = {}
        if args.payloads:
            for name in ("fresh_all", "fresh_rel", "followup", "toolhistory"):
                p = os.path.join(args.payloads, f"payload-{name}.json")
                if os.path.exists(p):
                    self.payloads[name] = load_payload(p)
        self.session_seq = 0
        # payload_key -> what was used instead, surfaced in the output so a
        # substitution can never be mistaken for the real thing.
        self.substituted: dict[str, str] = {}

    def new_session(self, tag: str) -> str:
        self.session_seq += 1
        return f"bakeoff-{tag}-{int(time.time())}-{self.session_seq}"

    # one turn, whichever transport
    def turn(self, message: str, *, payload_key: str = "fresh_rel",
             session: str | None = None, max_tokens: int | None = None) -> Turn:
        if self.a.mode == "giap":
            return stream_giap(self.a.base, message, session or self.new_session("t"), self.a.timeout)
        body = self.payloads.get(payload_key)
        if body is None:
            # Fall back rather than abandon the run: a follow-up replayed on the
            # fresh payload still exercises the server's own prefix cache, and
            # losing an hour of completed workloads to one absent file is worse
            # than a substitution that is recorded and visible in the report.
            body = self.payloads.get("fresh_rel")
            if body is None:
                raise SystemExit(
                    "--mode openai needs at least payload-fresh_rel.json. "
                    "Run capture-payload.sh first and pass --payloads DIR."
                )
            self.substituted.setdefault(payload_key, "fresh_rel")
        body = with_user_message(body, message)
        if self.a.model:
            body["model"] = self.a.model
        if max_tokens:
            body["max_tokens"] = max_tokens
        return stream_openai(self.a.base, body, self.a.timeout)

    def repeat(self, label: str, fn, n: int) -> dict:
        rows = []
        for i in range(n):
            t = fn(i)
            rows.append(t.as_dict())
            print(f"     {label} [{i+1}/{n}] {rows[-1]['outcome']}"
                  f" ttft={_fmt(rows[-1]['ttft_ms'])}ms"
                  f" decode={_fmt(rows[-1]['decode_tok_per_sec'])} tok/s", flush=True)
        return {
            "raw": rows,
            "ttft_ms": summarise([r["ttft_ms"] for r in rows]),
            "ttfs_ms": summarise([r["ttfs_ms"] for r in rows]),
            "decode_tok_per_sec": summarise([r["decode_tok_per_sec"] for r in rows]),
            "prompt_tokens": summarise([r["prompt_tokens"] for r in rows]),
            "wall_ms": summarise([r["wall_ms"] for r in rows]),
        }

    # -- individual workloads -------------------------------------------------
    def w_fresh(self, key: str) -> dict:
        return self.repeat(key, lambda i: self.turn("What is the weather right now?",
                                                    payload_key=key,
                                                    session=self.new_session(key)), self.a.repeats)

    def w_followup(self) -> dict:
        """Turn 2 of a live session: the KV-reuse case.

        Must reuse the SAME session id as its own turn 1, or there is no prefix
        to reuse and the number measures a fresh turn wearing a follow-up's name.
        """
        rows = []
        for i in range(self.a.repeats):
            sid = self.new_session("reuse")
            self.turn("What is the weather right now?", payload_key="fresh_rel", session=sid)
            t = self.turn("And tomorrow?", payload_key="followup", session=sid)
            rows.append(t.as_dict())
            print(f"     followup [{i+1}/{self.a.repeats}] ttft={_fmt(rows[-1]['ttft_ms'])}ms", flush=True)
        return {
            "raw": rows,
            "ttft_ms": summarise([r["ttft_ms"] for r in rows]),
            "decode_tok_per_sec": summarise([r["decode_tok_per_sec"] for r in rows]),
            "cached_prompt_tokens": summarise([r["cached_prompt_tokens"] for r in rows]),
        }

    def w_voice(self) -> dict:
        return self.repeat("voice", lambda i: self.turn("what time is it",
                                                        payload_key="fresh_rel",
                                                        session=self.new_session("voice"),
                                                        max_tokens=64), self.a.repeats)

    def w_decode(self) -> dict:
        return self.repeat("decode", lambda i: self.turn(
            "Count from 1 to 100, comma separated.", payload_key="fresh_rel",
            session=self.new_session("decode"), max_tokens=256), self.a.repeats)

    def w_reliability(self, key: str) -> dict:
        counts = {"tool_call_ok": 0, "wrong_tool": 0, "text_only": 0,
                  "bad_args": 0, "stream_error": 0, "empty": 0}
        rows = []
        for ask, expect in MUST_CALL:
            t = self.turn(ask, payload_key=key, session=self.new_session("rel"))
            d = t.as_dict()
            if d["outcome"] in ("stream_error", "bad_args", "empty"):
                cls = d["outcome"]
            elif not d["tools"]:
                cls = "text_only"
            elif any(expect.split("__")[-1] in name for name in d["tools"]):
                cls = "tool_call_ok"
            else:
                cls = "wrong_tool"
            counts[cls] += 1
            rows.append({"ask": ask, "expected": expect, "got": d["tools"], "class": cls,
                         "detail": d["detail"]})
            print(f"     rel[{key}] {cls:<13} {ask[:44]}", flush=True)
        return {"counts": counts, "of": len(MUST_CALL), "rows": rows}

    def w_must_not_call(self, key: str) -> dict:
        spurious, rows = 0, []
        for ask in MUST_NOT_CALL:
            t = self.turn(ask, payload_key=key, session=self.new_session("mnc"))
            d = t.as_dict()
            called = bool(d["tools"])
            spurious += int(called)
            rows.append({"ask": ask, "got": d["tools"], "spurious": called})
            print(f"     mnc {'CALLED ' if called else 'clean  '} {ask[:44]}", flush=True)
        return {"spurious": spurious, "of": len(MUST_NOT_CALL), "rows": rows}

    def w_canary(self, key: str) -> dict:
        hits, rows = 0, []
        for ask, want in CANARY:
            t = self.turn(ask, payload_key=key, session=self.new_session("canary"), max_tokens=96)
            ok = want.lower() in t.text.lower()
            hits += int(ok)
            rows.append({"ask": ask, "want": want, "ok": ok, "said": t.text[:120]})
        print(f"     canary {hits}/{len(CANARY)}", flush=True)
        return {"score": hits, "of": len(CANARY), "rows": rows}

    def w_multiturn(self, key: str) -> dict:
        """History carrying a prior assistant tool_call.

        This is the shape that broke a streaming `--jinja` tool-call parser
        before: the failure is not a wrong answer, it is an aborted stream.
        """
        errors, rows = 0, []
        for ask, _ in MUST_CALL[:5]:
            sid = self.new_session("multi")
            self.turn(ask, payload_key=key, session=sid)
            t = self.turn("Thanks. Now what is on my schedule?",
                          payload_key="toolhistory" if "toolhistory" in self.payloads else key,
                          session=sid)
            d = t.as_dict()
            broke = d["outcome"] in ("stream_error", "bad_args")
            errors += int(broke)
            rows.append({"opened_with": ask, "outcome": d["outcome"], "detail": d["detail"]})
            print(f"     multiturn {'BROKE' if broke else 'ok   '} {d['outcome']}", flush=True)
        return {"stream_errors": errors, "of": 5, "rows": rows}


def _fmt(v) -> str:
    return "  n/a" if v is None else f"{v:.1f}"


WORKLOADS = ["fresh_all", "fresh_rel", "followup", "voice", "decode",
             "reliability", "must_not_call", "canary", "multiturn"]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--mode", choices=["giap", "openai"], required=True)
    ap.add_argument("--base", required=True, help="http://127.0.0.1:8080 (giap) or :8001 (openai)")
    ap.add_argument("--model", default="", help="served model id (openai mode)")
    ap.add_argument("--payloads", default="", help="dir of payload-*.json from capture-payload.sh")
    ap.add_argument("--workloads", default="voice,followup,decode,fresh_rel",
                    help=f"comma-separated: {','.join(WORKLOADS)} (or 'all')")
    ap.add_argument("-r", "--repeats", type=int, default=5)
    ap.add_argument("--timeout", type=float, default=300.0)
    ap.add_argument("--out", default="")
    args = ap.parse_args()

    wanted = WORKLOADS if args.workloads == "all" else [w.strip() for w in args.workloads.split(",")]
    unknown = [w for w in wanted if w not in WORKLOADS]
    if unknown:
        raise SystemExit(f"unknown workload(s): {unknown}; known: {WORKLOADS}")

    d = Driver(args)
    out: dict = {"mode": args.mode, "base": args.base, "model": args.model,
                 "repeats": args.repeats, "workloads": {}}
    if args.mode == "openai":
        out["payloads_used"] = sorted(d.payloads)

    for w in wanted:
        print(f"\n-- {w}", flush=True)
        started = time.monotonic()
        try:
            _run_one(d, out, w, args)
        except Exception as e:  # noqa: BLE001 - a broken workload must not discard the rest
            print(f"     !! {w} failed: {e}", flush=True)
            out["workloads"][w] = {"error": str(e)}
        out["workloads"][w]["seconds"] = round(time.monotonic() - started, 1)
        # Write after every workload: a run interrupted at hour two still has
        # everything up to hour two.
        if args.out:
            with open(args.out, "w") as f:
                json.dump(out, f, indent=2)

    if d.substituted:
        out["payload_substitutions"] = d.substituted
        print(f"\nNOTE: payload substitutions in effect: {d.substituted}")

    text = json.dumps(out, indent=2)
    if args.out:
        with open(args.out, "w") as f:
            f.write(text)
        print(f"\nruns -> {args.out}")
    else:
        print(text)
    return 0


def _run_one(d: "Driver", out: dict, w: str, args) -> None:
        if w in ("fresh_all", "fresh_rel"):
            out["workloads"][w] = d.w_fresh(w)
        elif w == "followup":
            out["workloads"][w] = d.w_followup()
        elif w == "voice":
            out["workloads"][w] = d.w_voice()
        elif w == "decode":
            out["workloads"][w] = d.w_decode()
        elif w == "reliability":
            out["workloads"][w] = {
                "relevant": d.w_reliability("fresh_rel"),
                **({"all": d.w_reliability("fresh_all")} if "fresh_all" in d.payloads
                   or args.mode == "giap" else {}),
            }
        elif w == "must_not_call":
            out["workloads"][w] = d.w_must_not_call("fresh_rel")
        elif w == "canary":
            out["workloads"][w] = d.w_canary("fresh_rel")
        elif w == "multiturn":
            out["workloads"][w] = d.w_multiturn("fresh_rel")


if __name__ == "__main__":
    sys.exit(main())
