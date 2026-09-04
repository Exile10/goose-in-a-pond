#!/usr/bin/env python3
"""Interactive driver. Watch the two models hand work to each other.

    python3 chat.py                 dual-wield, interactive
    python3 chat.py --solo          single-model control arm
    python3 chat.py --both "..."    run one question through both arms
"""

import argparse
import sys
import time

from dualgemma import agent, tools
from dualgemma.llama import Llama

WHO = {"planner": "E2B     ", "picker": "FGEMMA  ", "tool": "TOOL    "}


def make_trace(enabled):
    if not enabled:
        return None

    def trace(who, kind, text):
        text = str(text).replace("\n", " ")
        if len(text) > 200:
            text = text[:200] + "..."
        print("  %s %-12s %s" % (WHO.get(who, who), kind, text))
    return trace


def connect(planner_port, picker_port, need_picker, think=True):
    planner = Llama("http://localhost:%d" % planner_port, "gemma-4-E2B-it", think=think)
    picker = Llama("http://localhost:%d" % picker_port, "functiongemma-270m")
    missing = []
    if not planner.health():
        missing.append("planner on :%d" % planner_port)
    if need_picker and not picker.health():
        missing.append("picker on :%d" % picker_port)
    if missing:
        sys.exit("not running: %s\nstart them with: ./serve.sh start" % ", ".join(missing))
    return planner, picker


def ask(question, arm, planner, picker, planner_think, verbose=True):
    tools.reset()
    started = time.time()
    trace = make_trace(verbose)
    if arm == "dual":
        result = agent.run_dual(planner, picker, question, trace=trace)
    elif arm == "natural":
        result = agent.run_natural(planner_think, picker, question, trace=trace)
    else:
        result = agent.run_solo(planner, question, trace=trace)
    elapsed = time.time() - started
    return result, elapsed


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("question", nargs="*", help="ask once and exit")
    parser.add_argument("--solo", action="store_true", help="single-model control arm")
    parser.add_argument("--natural", action="store_true",
                        help="natural-language arm: E2B asks in prose, picker fans out (thinking always on)")
    parser.add_argument("--both", action="store_true", help="dual + solo")
    parser.add_argument("--all", action="store_true", help="all three arms")
    parser.add_argument("--quiet", action="store_true", help="hide the inter-model trace")
    parser.add_argument("--no-think", action="store_true",
                        help="disable the planner's thinking channel (fixed per session)")
    parser.add_argument("--planner-port", type=int, default=8091)
    parser.add_argument("--picker-port", type=int, default=8092)
    args = parser.parse_args()

    if args.all:
        arms = ["dual", "solo", "natural"]
    elif args.both:
        arms = ["dual", "solo"]
    elif args.natural:
        arms = ["natural"]
    elif args.solo:
        arms = ["solo"]
    else:
        arms = ["dual"]
    needs_picker = "dual" in arms or "natural" in arms
    planner, picker = connect(args.planner_port, args.picker_port, needs_picker, think=not args.no_think)
    planner_think = Llama(planner.base_url, planner.name, think=True)

    def once(question):
        for arm in arms:
            print("\n[%s]" % arm)
            result, elapsed = ask(question, arm, planner, picker, planner_think, not args.quiet)
            print("  %s %-12s %s" % ("=>      ", "%.1fs" % elapsed, result.answer))

    if args.question:
        once(" ".join(args.question))
        return

    print("dual-wielding gemma. tools: %s" % ", ".join(t["name"] for t in tools.TOOLS))
    print("ctrl-c or 'exit' to quit.\n")
    while True:
        try:
            question = input("you> ").strip()
        except (EOFError, KeyboardInterrupt):
            print()
            break
        if not question:
            continue
        if question in ("exit", "quit"):
            break
        try:
            once(question)
        except Exception as exc:
            print("  error: %s" % exc)
        print()


if __name__ == "__main__":
    main()
