#!/usr/bin/env python3
"""
report.py — merge bake-off envelopes, apply the gates, rank the survivors.

    python3 report.py --in ~/bakeoff-results/2026-09-07 --out docs/developer/jetson-engine-bakeoff.md

Two rules this file exists to enforce, because both are easy to lose by hand:

1. A row without provenance is not reported. An envelope missing its gates,
   power state, or swap delta is listed as UNUSABLE, never quoted as a result.
   A decode rate taken at an unknown power mode is not a measurement.

2. A memory saving must be quality-backed. A configuration that claims less
   memory but has no reliability and canary row IN THAT SAME CONFIGURATION is
   printed as "unvalidated" -- never as a saving. That is the user's rule:
   memory pressure must be backed by the reliability of the results.
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import sys

# ── gate constants ───────────────────────────────────────────────────────────
# Reliability floors. NOTE the small-n caveat: 12 must-call turns cannot
# separate 11/12 from 12/12 with any confidence. These are floors for
# elimination, not a ranking signal; ties are broken on the combined total.
REL_MIN_RELEVANT = 0.90
REL_MIN_ALL = 0.83

# Memory reserve, in MB, that must remain available at the run's MINIMUM.
#
# `MemAvailable` is what is left after everything already running, so the OS and
# the server are on the spent side of the ledger, not the reserved side. Adding
# an OS allowance here double-counted them and failed the incumbent at 1,308 MB
# against a 2,312 MB bar that had ~1,500 MB of already-spent memory inside it.
# What must genuinely remain is what is not yet loaded: the voice stack, and
# enough contiguous room for a large CUDA allocation to succeed.
RESERVE_VOICE_FALLBACK_MB = 300
RESERVE_NVMAP_SLACK_MB = 512

# Swap tolerance. The principle is that a model served from swap reads as a slow
# model and never as an error, so swap use is disqualifying -- but SwapFree also
# drifts by a few MB from unrelated background activity over an hour-long run.
# A model actually spilling shows hundreds of MB, so the line goes where those
# two cannot be confused rather than at a literal zero.
SWAP_NOISE_MB = 128

# Memory bandwidth ceiling: decode is bandwidth-bound, so tok/s can never
# exceed roughly (GB/s) / (weights GB). A number above it is a measurement bug,
# not a fast engine.
BANDWIDTH_GBPS = 102.0

# Significance band for the lexicographic ranking.
BAND = 0.10
# An HTTP sidecar must clear this to pay for its own integration cost.
CHALLENGER_MARGIN = 0.10
CHALLENGER_MAX_REGRESSION = 0.05


def load(indir: str) -> list[dict]:
    out = []
    for p in sorted(glob.glob(os.path.join(indir, "**", "envelope.json"), recursive=True)):
        try:
            e = json.load(open(p))
            e["_path"] = p
            out.append(e)
        except (OSError, json.JSONDecodeError) as ex:
            print(f"skipping unreadable {p}: {ex}", file=sys.stderr)
    return out


def med(env: dict, workload: str, metric: str):
    w = (env.get("runs") or {}).get("workloads", {}).get(workload) or {}
    m = w.get(metric)
    return m.get("median") if isinstance(m, dict) else None


def ttft(env: dict, workload: str):
    """Time to first token, comparable ACROSS transports.

    For an HTTP replay one request is one inference, so client-side first-delta
    IS the engine's TTFT. For GIAP a turn is 2-3 inferences plus reasoning plus a
    tool round-trip, and its turn latency is 19-36 s against an engine TTFT near
    1 s. Ranking the two side by side reported C2 as 98% faster, which is not an
    engine result -- it is one number measuring a turn and another measuring an
    inference. Prefer the engine's own figure wherever the candidate reports one.
    """
    return med(env, workload, "engine_ttft_ms") or med(env, workload, "ttft_ms")


def turn_latency(env: dict, workload: str):
    """What a user waits, end to end. Only defined for a candidate that runs the
    whole agent loop, so it ranks nothing -- it is reported alongside."""
    return med(env, workload, "ttft_ms")


def spread(env: dict, workload: str, metric: str):
    w = (env.get("runs") or {}).get("workloads", {}).get(workload) or {}
    m = w.get(metric)
    return m.get("spread_pct") if isinstance(m, dict) else None


def reliability(env: dict) -> dict:
    """Scores per arm.

    The `relevant` arm is NOT meaningful for a replayed payload. A narrowed
    capture froze its tool set around the question it was captured for, so
    substituting a different ask leaves the model graded on whether it called a
    tool that is not in the array. Measured: every C2 variant scored 3/10 on
    `relevant` and 9-10/10 on `all`, identically, which is the payload speaking
    and not the engine. Only live-selection candidates can be gated on it.
    """
    rel = (env.get("runs") or {}).get("workloads", {}).get("reliability") or {}
    out = {}
    for arm in ("relevant", "all"):
        a = rel.get(arm)
        if not a:
            continue
        c, n = a.get("counts", {}), a.get("of", 0) or 1
        out[arm] = {
            "ok": c.get("tool_call_ok", 0),
            "of": n,
            "rate": c.get("tool_call_ok", 0) / n,
            "stream_error": c.get("stream_error", 0),
            "bad_args": c.get("bad_args", 0),
        }
    return out


def apply_gates(env: dict, reserve_mb: int) -> dict:
    """Return {gate: (bool|None, reason)}. None = could not be evaluated."""
    g: dict = {}
    runs = (env.get("runs") or {}).get("workloads", {})
    mem = env.get("memory") or {}

    # G0 functional: did anything actually stream?
    any_ok = False
    for w in runs.values():
        for r in (w.get("raw") or []):
            if r.get("outcome") == "ok" and (r.get("ttft_ms") is not None):
                any_ok = True
    g["G0_functional"] = (any_ok, "streamed at least one turn with content"
                          if any_ok else "no turn produced content")

    # G1 reliability
    rel = reliability(env)
    if not rel:
        g["G1_reliability"] = (None, "reliability workload not run")
    else:
        replayed = env.get("candidate", "").startswith(("c2-", "c3-", "c4-"))
        reasons, ok = [], True
        for arm, floor in (("relevant", REL_MIN_RELEVANT), ("all", REL_MIN_ALL)):
            if arm not in rel:
                continue
            if arm == "relevant" and replayed:
                reasons.append(f"relevant {rel[arm]['ok']}/{rel[arm]['of']} NOT GATED "
                               f"(frozen payload: its tools were selected for a different ask)")
                continue
            r = rel[arm]
            if r["rate"] < floor:
                ok = False
                reasons.append(f"{arm} {r['ok']}/{r['of']} < {floor:.0%}")
            if r["stream_error"]:
                ok = False
                reasons.append(f"{arm} {r['stream_error']} stream error(s)")
        mt = runs.get("multiturn") or {}
        if mt.get("stream_errors"):
            ok = False
            reasons.append(f"multiturn {mt['stream_errors']}/{mt.get('of')} broke the stream")
        g["G1_reliability"] = (ok, "; ".join(reasons) or "within floors")

    # G2 memory. A swap delta is disqualifying on its own: part of the run was
    # served from swap, which reads as a slow model and never as an error.
    swap = mem.get("swap_delta_mb")
    avail_min = mem.get("mem_available_min_mb")
    if swap is None or avail_min is None:
        g["G2_memory"] = (None, "no memwatch summary (swap_delta_mb / mem_available_min_mb absent)")
    else:
        reasons, ok = [], True
        if swap > SWAP_NOISE_MB:
            ok = False
            reasons.append(f"{swap} MB of swap was consumed — part of the run was served from swap")
        elif swap > 0:
            reasons.append(f"{swap} MB swap drift (under the {SWAP_NOISE_MB} MB noise floor)")
        if avail_min < reserve_mb:
            ok = False
            reasons.append(f"MemAvailable fell to {avail_min} MB, below the {reserve_mb} MB reserve")
        lfb = mem.get("lfb_min_mb")
        if lfb is not None and lfb < 4:
            reasons.append(f"largest free block fell to {lfb} MB (fragmentation warning, not a failure)")
        g["G2_memory"] = (ok, "; ".join(reasons) or f"peak left {avail_min} MB available")

    # G3 quality-backed. Only meaningful for a variant claiming a saving.
    canary = runs.get("canary")
    if env.get("variant", "baseline") == "baseline":
        g["G3_quality_backed"] = (True, "baseline: nothing claimed")
    elif not canary or not rel:
        g["G3_quality_backed"] = (False,
                                  "a variant claiming a saving was not scored for reliability "
                                  "and quality in that same configuration")
    else:
        ok = canary["score"] >= canary["of"] - 1
        g["G3_quality_backed"] = (ok, f"canary {canary['score']}/{canary['of']}")
    return g


def draft_stats(env: dict) -> dict | None:
    """llama.cpp's per-request `timings`, when the run used speculative decoding."""
    for w in ("decode", "voice", "fresh_rel"):
        for r in ((env.get("runs") or {}).get("workloads", {}).get(w) or {}).get("raw") or []:
            t = r.get("timings") or {}
            if t.get("draft_n"):
                return t
    return None


def sanity(env: dict) -> list[str]:
    out = []
    wts = (env.get("model") or {}).get("bytes")
    dec = med(env, "decode", "decode_tok_per_sec")
    spec = draft_stats(env)
    if wts and dec:
        ceiling = BANDWIDTH_GBPS / (wts / 1e9)
        if dec > ceiling and spec:
            # The ceiling bounds TARGET forward passes. Speculative decoding emits
            # several accepted tokens per pass, so exceeding it is the mechanism
            # working, not a broken measurement -- provided the acceptance rate is
            # there to explain it.
            acc = spec["draft_n_accepted"] / max(spec["draft_n"], 1)
            out.append(f"decode {dec:.1f} tok/s is above the {ceiling:.1f} tok/s single-stream "
                       f"ceiling, explained by speculative decoding: "
                       f"{spec['draft_n_accepted']}/{spec['draft_n']} drafted tokens accepted "
                       f"({acc:.0%}); the server's own timings report "
                       f"{spec.get('predicted_per_second', 0):.1f} tok/s independently")
        elif dec > ceiling:
            out.append(f"decode {dec:.1f} tok/s exceeds the {ceiling:.1f} tok/s bandwidth ceiling "
                       f"for {wts/1e9:.2f} GB of weights, with NO speculative decoding to explain "
                       f"it — this is a measurement bug, not a fast engine")
    for w in ("voice", "followup", "decode", "fresh_rel"):
        s = spread(env, w, "decode_tok_per_sec") or spread(env, w, "ttft_ms")
        if s is not None and s > 15:
            out.append(f"{w}: {s:.0f}% spread across repeats — the runs were not in the same state")
    p = env.get("power") or {}
    if not p.get("nvpmodel_name"):
        out.append("power mode was not recorded; nothing here is comparable with another candidate")
    if p.get("oc3_delta") and p.get("oc3_seconds"):
        rate = p["oc3_delta"] * 60.0 / max(p["oc3_seconds"], 1)
        out.append(f"over-current: {rate:.0f} oc3 events/min at {p.get('nvpmodel_name')}")
    return out


def usable(env: dict, gates: dict) -> tuple[bool, str]:
    if not (env.get("power") or {}).get("nvpmodel_name"):
        return False, "no power state recorded"
    if (env.get("memory") or {}).get("swap_delta_mb") is None:
        return False, "no swap delta recorded"
    if not gates:
        return False, "no gates evaluated"
    return True, ""


def rank(cands: list[dict]) -> list[dict]:
    """Lexicographic with a significance band: a rung only decides when the gap is real."""
    rungs = [("voice", "ttft", False),
             ("followup", "ttft", False),
             ("decode", "decode_tok_per_sec", True),
             ("fresh_rel", "ttft", False)]

    def better(a, b) -> int:
        for w, m, higher in rungs:
            if m == "ttft":
                va, vb = ttft(a, w), ttft(b, w)
            else:
                va, vb = med(a, w, m), med(b, w, m)
            if va is None or vb is None or not va or not vb:
                continue
            gap = abs(va - vb) / max(va, vb)
            if gap < BAND:
                continue
            if higher:
                return -1 if va > vb else 1
            return -1 if va < vb else 1
        aa = (a.get("memory") or {}).get("mem_available_min_mb") or 0
        bb = (b.get("memory") or {}).get("mem_available_min_mb") or 0
        return -1 if aa > bb else (1 if aa < bb else 0)

    import functools
    return sorted(cands, key=functools.cmp_to_key(better))


def fmt(v, unit="", nd=1):
    return "—" if v is None else f"{v:.{nd}f}{unit}"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--in", dest="indir", required=True)
    ap.add_argument("--out", default="")
    ap.add_argument("--voice-residency-mb", type=int, default=0,
                    help="measured ASR+TTS resident MB; 0 uses the documented fallback")
    args = ap.parse_args()

    voice_mb = args.voice_residency_mb or RESERVE_VOICE_FALLBACK_MB
    reserve = voice_mb + RESERVE_NVMAP_SLACK_MB

    envs = load(args.indir)
    if not envs:
        print(f"no envelopes under {args.indir}", file=sys.stderr)
        return 1

    L: list[str] = []
    w = L.append
    w("# Jetson inference-engine bake-off\n")
    w(f"Envelopes: `{args.indir}`  ·  candidates: {len(envs)}\n")
    w(f"Memory reserve for G2: **{reserve} MB** "
      f"({voice_mb} voice"
      f"{' (fallback, not measured)' if not args.voice_residency_mb else ' (measured)'}"
      f" + {RESERVE_NVMAP_SLACK_MB} NvMap contiguous slack)\n")

    scored, unusable = [], []
    for e in envs:
        e["_gates"] = apply_gates(e, reserve)
        e["_sanity"] = sanity(e)
        ok, why = usable(e, e["_gates"])
        (scored if ok else unusable).append((e, why))

    if unusable:
        w("\n## Unusable rows\n")
        w("These are not results. A number without the state it was taken in cannot be compared.\n")
        w("\n| candidate | variant | why |\n|---|---|---|")
        for e, why in unusable:
            w(f"| {e.get('candidate')} | {e.get('variant')} | {why} |")

    w("\n## Gates\n")
    w("| candidate | variant | model | G0 | G1 reliability | G2 memory | G3 quality-backed |")
    w("|---|---|---|---|---|---|---|")
    for e, _ in scored:
        g = e["_gates"]
        def cell(k):
            v, why = g.get(k, (None, "not evaluated"))
            mark = {True: "PASS", False: "**FAIL**", None: "n/a"}[v]
            return f"{mark} <br><sub>{why}</sub>"
        model = os.path.basename((e.get("model") or {}).get("file") or "?")
        w(f"| {e.get('candidate')} | {e.get('variant')} | {model} | {cell('G0_functional')} "
          f"| {cell('G1_reliability')} | {cell('G2_memory')} | {cell('G3_quality_backed')} |")

    w("\n## Measurements\n")
    w("Client-side timings throughout: from just before the request to the first chunk carrying "
      "content. Engine-reported numbers, where an engine reports any, are in the envelopes.\n")
    w("\n| candidate | variant | engine TTFT | turn latency | follow-up | decode | avail min | swap Δ | power |")
    w("|---|---|---:|---:|---:|---:|---:|---:|---|")
    for e, _ in scored:
        mem, p = e.get("memory") or {}, e.get("power") or {}
        w(f"| {e.get('candidate')} | {e.get('variant')} "
          f"| {fmt(ttft(e,'voice'),' ms',0)} "
          f"| {fmt(turn_latency(e,'voice'),' ms',0)} "
          f"| {fmt(ttft(e,'followup'),' ms',0)} "
          f"| {fmt(med(e,'decode','decode_tok_per_sec'),' tok/s')} "
          f"| {mem.get('mem_available_min_mb','—')} MB "
          f"| {mem.get('swap_delta_mb','—')} MB "
          f"| {p.get('nvpmodel_name','?')}/{p.get('jetson_clocks','?')} |")

    # Parity: the check that makes the whole comparison interpretable.
    c1 = next((e for e, _ in scored if e.get("candidate") == "c1-giap"), None)
    c2 = next((e for e, _ in scored if e.get("candidate") == "c2-llama-server"
               and e.get("variant") == "baseline"), None)
    if c1 and c2:
        w("\n## C1 vs C2-baseline parity\n")
        w("Same model, same quant, mirrored flags. A gap here is engine vintage plus GIAP's own "
          "per-token overhead; anything larger than the tolerance means the two are not running "
          "the same configuration and no other comparison in this report is safe.\n")
        w("\n| metric | C1 | C2-baseline | delta | tolerance | verdict |")
        w("|---|---:|---:|---:|---|---|")
        for label, wl, m, tol in (("decode tok/s", "decode", "decode_tok_per_sec", 0.10),
                                  ("engine TTFT", "voice", "ttft", 0.15),
                                  ("prompt tokens", "fresh_rel", "prompt_tokens", 0.03)):
            a, b = (ttft(c1, wl), ttft(c2, wl)) if m == "ttft" else (med(c1, wl, m), med(c2, wl, m))
            if a and b:
                d = (b - a) / a
                v = "ok" if abs(d) <= tol else "**OUT OF TOLERANCE**"
                w(f"| {label} | {a:.1f} | {b:.1f} | {d:+.1%} | ±{tol:.0%} | {v} |")
            else:
                w(f"| {label} | {fmt(a)} | {fmt(b)} | — | ±{tol:.0%} | not measured |")

    survivors = [e for e, _ in scored
                 if all(v is not False for v, _ in e["_gates"].values())]
    w("\n## Ranking\n")
    if not survivors:
        w("No candidate passed every gate. Nothing is recommended.\n")
    else:
        order = rank(survivors)
        w("Lexicographic, 10% significance band: voice TTFT → follow-up TTFT → decode → fresh TTFT "
          "→ memory headroom. A rung only decides when the gap exceeds the band.\n")
        for i, e in enumerate(order, 1):
            w(f"{i}. **{e.get('candidate')}** ({e.get('variant')}) — "
              f"{os.path.basename((e.get('model') or {}).get('file') or '?')}")
        if order[0].get("candidate") != "c1-giap" and c1 is not None:
            top = order[0]
            v_t, v_c = ttft(top, "voice"), ttft(c1, "voice")
            d_t, d_c = med(top, "decode", "decode_tok_per_sec"), med(c1, "decode", "decode_tok_per_sec")
            gain = max((v_c - v_t) / v_c if v_c and v_t else 0,
                       (d_t - d_c) / d_c if d_c and d_t else 0)
            w(f"\n**Challenger check.** {top.get('candidate')} leads, but a sidecar has a standing "
              f"cost (a second supervised process, wall-clock telemetry, re-solving the "
              f"sacrificial-context problem server-side). It must beat C1 by ≥{CHALLENGER_MARGIN:.0%} "
              f"on TTFT or decode with no rung worse by >{CHALLENGER_MAX_REGRESSION:.0%}. "
              f"Best gain measured: **{gain:+.1%}** — "
              f"{'clears the bar' if gain >= CHALLENGER_MARGIN else 'does NOT clear the bar; keep C1'}.\n")

    notes = [(e, e["_sanity"]) for e, _ in scored if e["_sanity"]]
    if notes:
        w("\n## Sanity checks and warnings\n")
        for e, ss in notes:
            w(f"\n**{e.get('candidate')} / {e.get('variant')}**\n")
            for s in ss:
                w(f"- {s}")
        for e, _ in scored:
            for warning in e.get("warnings") or []:
                w(f"- _{e.get('candidate')}_: {warning}")

    text = "\n".join(L) + "\n"
    if args.out:
        os.makedirs(os.path.dirname(os.path.abspath(args.out)) or ".", exist_ok=True)
        open(args.out, "w").write(text)
        print(f"report -> {args.out}")
    else:
        print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
