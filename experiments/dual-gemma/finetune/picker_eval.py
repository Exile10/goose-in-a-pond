#!/usr/bin/env python3
"""Score a FunctionGemma endpoint on the held-out labelled set, in isolation.

    python3 picker_eval.py [--port 8092] [--label base]

No planner, no loop: each case's instruction goes straight to the picker
with the full real-tool declaration block, output is parsed with the same
head-run rule the agent uses, and the parsed calls are compared against
the labels. This is the before/after instrument for the fine-tune.

Metrics per case:
  calls    every expected call present (args as subset), no wrong-tool extras
  stop     generation ended by itself (eos/stop-word) rather than the cap --
           the base model babbles to the token limit every single time
"""

import argparse
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from dualgemma import fgemma, tools as toolbox  # noqa: E402
from dualgemma.llama import Llama  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))


def norm(value):
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return float(value)
    try:
        return float(str(value).strip())
    except ValueError:
        return str(value).strip().lower()


def subset(expected, actual):
    return all(k in actual and norm(v) == norm(actual[k]) for k, v in expected.items())


def score_case(case, calls):
    made = list(calls)
    matched = 0
    for want_tool, want_args in case["expect"]:
        for i, (tool, args) in enumerate(made):
            if tool == want_tool and subset(want_args, args):
                matched += 1
                made.pop(i)
                break
    expected_n = len(case["expect"])
    wrong_extras = sum(1 for tool, _ in made
                       if tool not in {t for t, _ in case["expect"]})
    return matched == expected_n and wrong_extras == 0


def run(port, label, verbose=True):
    picker = Llama("http://localhost:%d" % port, label)
    if not picker.health():
        sys.exit("no picker on :%d" % port)

    cases = [json.loads(line) for line in open(os.path.join(HERE, "data", "picker_eval.jsonl"))]
    ok = stopped = 0
    rows = []
    for case in cases:
        prompt = fgemma.build_prompt(case["instruction"], toolbox.TOOLS)
        t0 = time.time()
        raw = picker.complete(prompt, temperature=0.0, max_tokens=192,
                              stop=fgemma.MULTI_STOPS)
        dt = time.time() - t0
        calls, _err = fgemma.parse_calls(raw)
        good = score_case(case, calls)
        # llama-server tells us how generation ended via the raw length:
        # if it produced fewer tokens than the cap AND our stop strings/eos
        # fired, the model ended the turn itself.
        ended = len(raw) < 900 and ("<end_of_turn>" not in raw)  # heuristic fallback
        ok += good
        rows.append({"instruction": case["instruction"], "ok": good,
                     "calls": [[t, a] for t, a in calls], "seconds": round(dt, 2)})
        if verbose:
            print("  %-4s %5.1fs  %r" % ("ok" if good else "MISS", dt, case["instruction"][:58]))
            if not good:
                print("        got: %s" % json.dumps([[t, a] for t, a in calls])[:110])
    print("\n  %s: %d/%d cases correct (%.0f%%)" % (label, ok, len(cases), 100.0 * ok / len(cases)))
    return {"label": label, "ok": ok, "total": len(cases), "rows": rows}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=8092)
    parser.add_argument("--label", default="base")
    parser.add_argument("--out")
    opts = parser.parse_args()
    result = run(opts.port, opts.label)
    if opts.out:
        with open(opts.out, "w") as fh:
            json.dump(result, fh, indent=2)
        print("  wrote %s" % opts.out)


if __name__ == "__main__":
    main()
