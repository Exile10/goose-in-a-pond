#!/usr/bin/env python3
"""Score both arms over cases.json.

    python3 eval.py                  both arms, all cases
    python3 eval.py --arm dual
    python3 eval.py --repeat 3       average over repeats
    python3 eval.py --case timer_minutes

Three things are scored per case:
  tool     did the first call name the right tool (or correctly call none)
  args     did the required argument values come out right
  answer   does the final reply contain the facts it should
Plus mechanical failures: malformed calls the loop had to reject.
"""

import argparse
import json
import os
import sys
import time

from dualgemma import agent, tools
from dualgemma.llama import Llama

HERE = os.path.dirname(os.path.abspath(__file__))


def norm(value):
    if isinstance(value, bool):
        return value
    if isinstance(value, (int, float)):
        return float(value)
    text = str(value).strip().lower()
    try:
        return float(text)
    except ValueError:
        return text


def args_match(expected, actual):
    """Every expected key must be present and equal. Extra keys are fine."""
    for key, want in expected.items():
        if key not in actual:
            return False
        if norm(want) != norm(actual[key]):
            return False
    return True


def score(case, result):
    # Compound cases: every expected call must appear somewhere in the run
    # (order-insensitive; expected args are a subset of the actual ones).
    # Sequential arms satisfy this across several steps, the natural arm in
    # one fan-out; the metric is indifferent to which.
    if case.get("expect_calls"):
        made = [(c["tool"], c["args"]) for c in result.calls]
        matched = 0
        for want in case["expect_calls"]:
            for tool, args in made:
                if tool == want["tool"] and args_match(want.get("args", {}), args):
                    matched += 1
                    break
        tool_ok = {c["tool"] for c in result.calls} >= {w["tool"] for w in case["expect_calls"]}
        args_ok = matched == len(case["expect_calls"])
        answer = (result.answer or "").lower()
        wanted = case.get("answer_contains", [])
        answer_ok = all(str(w).lower() in answer for w in wanted) if wanted else None
        return {"tool": tool_ok, "args": args_ok, "answer": answer_ok,
                "malformed": len(result.errors), "n_calls": len(result.calls),
                "got": result.first_call}

    expect_tool = case.get("expect_tool")
    allowed = set(filter(None, [expect_tool] + case.get("alt_tools", [])))
    first = result.first_call

    if expect_tool is None:
        tool_ok = not result.calls
        args_ok = tool_ok
    else:
        tool_ok = bool(first) and first["tool"] in allowed
        expected_args = case.get("expect_args", {})
        if not expected_args:
            args_ok = tool_ok
        else:
            args_ok = tool_ok and first["tool"] == expect_tool and args_match(
                expected_args, first["args"])

    answer = (result.answer or "").lower()
    wanted = case.get("answer_contains", [])
    answer_ok = all(str(w).lower() in answer for w in wanted) if wanted else None

    return {
        "tool": tool_ok,
        "args": args_ok,
        "answer": answer_ok,
        "malformed": len(result.errors),
        "n_calls": len(result.calls),
        "got": first,
    }


def mark(value):
    if value is None:
        return " - "
    return " ok" if value else "MISS"


def run_arm(arm, cases, planner, picker, repeat, verbose, planner_think=None):
    rows = []
    for case in cases:
        for attempt in range(repeat):
            tools.reset()
            started = time.time()
            try:
                if arm == "dual":
                    result = agent.run_dual(planner, picker, case["question"])
                elif arm == "natural":
                    result = agent.run_natural(planner_think, picker, case["question"])
                else:
                    result = agent.run_solo(planner, case["question"])
            except Exception as exc:
                print("  %-22s CRASH %s" % (case["id"], exc))
                continue
            row = score(case, result)
            honoured, measurable = result.agreement
            row.update(id=case["id"], attempt=attempt, seconds=time.time() - started,
                       answer_text=result.answer, handoffs=result.handoffs,
                       honoured=honoured, measurable=measurable)
            rows.append(row)
            if verbose:
                got = row["got"]
                shown = "%s(%s)" % (got["tool"], json.dumps(got["args"])) if got else "(no call)"
                print("  %-22s tool %s  args %s  ans %s  %5.1fs  %s"
                      % (case["id"], mark(row["tool"]), mark(row["args"]),
                         mark(row["answer"]), row["seconds"], shown[:70]))
    return rows


def summarise(arm, rows, llamas):
    total = len(rows)
    if not total:
        return {}
    answered = [r for r in rows if r["answer"] is not None]
    summary = {
        "arm": arm,
        "cases": total,
        "tool": sum(1 for r in rows if r["tool"]),
        "args": sum(1 for r in rows if r["args"]),
        "answer": sum(1 for r in answered if r["answer"]),
        "answer_of": len(answered),
        "malformed": sum(r["malformed"] for r in rows),
        "seconds": round(sum(r["seconds"] for r in rows) / total, 2),
        "honoured": sum(r.get("honoured", 0) for r in rows),
        "measurable": sum(r.get("measurable", 0) for r in rows),
        "models": [m.stats() for m in llamas],
    }
    return summary


def print_summary(summary):
    pct = lambda n, d: "%3d/%-3d (%3.0f%%)" % (n, d, 100.0 * n / d) if d else "  n/a"
    print("\n  %-8s  tool %s   args %s   answer %s   malformed %-3d   %.1fs/case"
          % (summary["arm"],
             pct(summary["tool"], summary["cases"]),
             pct(summary["args"], summary["cases"]),
             pct(summary["answer"], summary["answer_of"]),
             summary["malformed"],
             summary["seconds"]))
    if summary.get("measurable"):
        honoured, measurable = summary["honoured"], summary["measurable"]
        print("            picker honoured the tool the planner named: %d/%d (%.0f%%)"
              % (honoured, measurable, 100.0 * honoured / measurable))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--arm", choices=["dual", "solo", "natural", "both", "all"],
                        default="both")
    parser.add_argument("--repeat", type=int, default=1)
    parser.add_argument("--case", action="append", help="run only these case ids")
    parser.add_argument("--quiet", action="store_true")
    parser.add_argument("--out", help="write per-case results as JSON here")
    parser.add_argument("--no-think", action="store_true",
                        help="disable the planner's thinking channel (fixed per session)")
    parser.add_argument("--planner-port", type=int, default=8091)
    parser.add_argument("--picker-port", type=int, default=8092)
    opts = parser.parse_args()

    with open(os.path.join(HERE, "cases.json")) as handle:
        cases = json.load(handle)
    if opts.case:
        wanted = set(opts.case)
        cases = [c for c in cases if c["id"] in wanted]
        if not cases:
            sys.exit("no cases matched %s" % ", ".join(sorted(wanted)))

    planner = Llama("http://localhost:%d" % opts.planner_port, "gemma-4-E2B-it", think=not opts.no_think)
    # The natural arm keeps thinking ON regardless of --no-think: its design
    # is deliberation plus a forgiving interface.
    planner_think = Llama("http://localhost:%d" % opts.planner_port, "gemma-4-E2B-it", think=True)
    picker = Llama("http://localhost:%d" % opts.picker_port, "functiongemma-270m")
    if opts.arm == "both":
        arms = ["dual", "solo"]
    elif opts.arm == "all":
        arms = ["dual", "solo", "natural"]
    else:
        arms = [opts.arm]
    if not planner.health():
        sys.exit("planner not up on :%d -- run ./serve.sh start" % opts.planner_port)
    if ("dual" in arms or "natural" in arms) and not picker.health():
        sys.exit("picker not up on :%d -- run ./serve.sh start" % opts.picker_port)

    everything, summaries = {}, []
    for arm in arms:
        print("\n== %s ==" % arm)
        for client in (planner, planner_think, picker):
            client.calls = 0
            client.seconds = 0.0
            client.prompt_tokens = client.completion_tokens = 0
            client.truncated = 0
        rows = run_arm(arm, cases, planner, picker, opts.repeat, not opts.quiet,
                       planner_think)
        everything[arm] = rows
        if arm == "dual":
            used = [planner, picker]
        elif arm == "natural":
            used = [planner_think, picker]
        else:
            used = [planner]
        summaries.append(summarise(arm, rows, used))

    print("\n" + "=" * 72)
    for summary in summaries:
        print_summary(summary)
    print("=" * 72)

    if opts.out:
        with open(opts.out, "w") as handle:
            json.dump({"summaries": summaries, "rows": everything}, handle, indent=2)
        print("wrote %s" % opts.out)


if __name__ == "__main__":
    main()
