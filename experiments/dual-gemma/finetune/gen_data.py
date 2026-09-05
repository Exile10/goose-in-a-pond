#!/usr/bin/env python3
"""Synthesize FunctionGemma fine-tuning data from the measured failure classes.

    python3 gen_data.py [--train N] [--seed S]

Writes mlx-lm "completions" JSONL (prompt/completion pairs, loss on the
completion) to data/train.jsonl and data/valid.jsonl, plus a labelled
data/picker_eval.jsonl the trainer never sees, for before/after scoring.

Every example teaches one or more of the behaviours the experiment measured
as broken (see ../README.md):

  stop        end the turn after the calls -- the base model babbles forever
  register    pseudo-code input (tool(600, x) / k=v) must transduce the same
              as prose; the base model corrupts numbers fed as code
  fidelity    multi-digit numbers are copied exactly, never invented
  near-miss   an instruction naming another tool's domain word (time, note)
              must not flip the pick (get_time vs read_note is the measured
              killer)
  exact-name  "my jetson note" -> name jetson, not "jetson note"
  fan-out     compound instructions become 2-3 tagged calls, in order

Half the examples use procedurally generated synthetic tools so the
behaviours generalize past this bench's eight; the eval set is entirely
real-tool and includes the verbatim measured failures.
"""

import argparse
import json
import os
import random
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from dualgemma import fgemma, tools as toolbox  # noqa: E402

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data")


# --- rendering ----------------------------------------------------------

def render_value(value):
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    return "<escape>%s<escape>" % value


def render_call(name, args):
    body = ",".join("%s:%s" % (k, render_value(v)) for k, v in args.items())
    return "<start_function_call>call:%s{%s}<end_function_call>" % (name, body)


def example(instruction, calls, tool_set):
    """One training pair in FunctionGemma's native format.

    The completion ends with <end_of_turn>: teaching the stop is half the
    point of the whole fine-tune.
    """
    return {
        "prompt": fgemma.build_prompt(instruction, tool_set),
        "completion": "".join(render_call(n, a) for n, a in calls) + "<end_of_turn>",
    }


# --- synthetic tools ----------------------------------------------------

VERBS = ["get", "set", "list", "read", "send", "start", "stop", "check",
         "find", "update", "clear", "log"]
NOUNS = ["lamp", "fan", "door", "playlist", "alarm", "route", "invoice",
         "ticket", "batch", "sensor", "backup", "queue", "report", "shift"]
PARAM_NAMES = ["name", "id", "count", "level", "duration", "target", "zone",
               "label", "amount", "channel", "priority", "mode"]

CITIES = ["Nairobi", "Kisumu", "Mombasa", "Eldoret", "Nakuru", "London",
          "Tokyo", "Kigali", "Kampala", "Dodoma", "Cairo", "Accra"]
WORDS = ["standup", "tea", "oven", "laundry", "backup", "meeting", "lunch",
         "workout", "review", "deploy", "rice", "garden"]


def synth_tool(rng):
    name = "%s_%s" % (rng.choice(VERBS), rng.choice(NOUNS))
    n_params = rng.randint(0, 3)
    params, required = {}, []
    for pname in rng.sample(PARAM_NAMES, n_params):
        ptype = rng.choice(["string", "integer", "integer", "number"])
        params[pname] = {"type": ptype, "description": pname.replace("_", " ")}
        if rng.random() < 0.7:
            required.append(pname)
    return {"name": name, "description": name.replace("_", " "),
            "parameters": params, "required": required}


def synth_toolset(rng, k=None):
    seen, out = set(), []
    while len(out) < (k or rng.randint(3, 8)):
        tool = synth_tool(rng)
        if tool["name"] not in seen:
            seen.add(tool["name"])
            out.append(tool)
    return out


def synth_args(tool, rng):
    args = {}
    for pname, spec in tool["parameters"].items():
        if pname not in tool["required"] and rng.random() < 0.4:
            continue
        if spec["type"] == "string":
            args[pname] = rng.choice(WORDS + CITIES)
        else:
            # varied digit counts: number fidelity is a target behaviour
            args[pname] = rng.choice([rng.randint(1, 9), rng.randint(10, 99),
                                      rng.randint(100, 999), rng.randint(1000, 99999)])
    return args


# --- instruction phrasings ---------------------------------------------

def prose(tool, args, rng):
    """A plain-English rendering of one intended call."""
    bits = []
    for key, value in args.items():
        if isinstance(value, str):
            bits.append("%s %s" % (key, value))
        else:
            bits.append("%s %s" % (key, value))
    head = rng.choice(["Please %s", "%s", "Now %s", "Go ahead and %s"]) % (
        tool["name"].replace("_", " "))
    if not bits:
        return head
    return "%s with %s" % (head, rng.choice([", ".join(bits), " and ".join(bits)]))


def pseudo_code(tool, args, rng):
    """The register E2B drifts into and the base model misreads."""
    style = rng.random()
    if style < 0.4:      # positional -- the worst measured case
        rendered = ", ".join(str(v) for v in args.values())
    elif style < 0.8:    # keyword
        rendered = ", ".join("%s=%s" % (k, v) for k, v in args.items())
    else:                # keyword with quotes
        rendered = ", ".join(
            '%s="%s"' % (k, v) if isinstance(v, str) else "%s=%s" % (k, v)
            for k, v in args.items())
    return "%s(%s)" % (tool["name"], rendered)


# --- generators, one per failure class ----------------------------------

def gen_single(rng, real):
    pool = toolbox.TOOLS if real else synth_toolset(rng)
    tool = rng.choice([t for t in pool if t["parameters"]] or pool)
    args = real_args(tool, rng) if real else synth_args(tool, rng)
    phrasing = rng.choice([prose, prose, pseudo_code])  # code register: 1 in 3
    return example(phrasing(tool, args, rng), [(tool["name"], args)], pool)


def gen_compound(rng, real):
    pool = toolbox.TOOLS if real else synth_toolset(rng, k=rng.randint(4, 8))
    k = rng.choice([2, 2, 3])
    chosen = rng.sample(pool, min(k, len(pool)))
    calls, phrases = [], []
    for tool in chosen:
        args = real_args(tool, rng) if real else synth_args(tool, rng)
        calls.append((tool["name"], args))
        phrases.append(rng.choice([prose, prose, pseudo_code])(tool, args, rng))
    joiner = rng.choice([" and ", " and also ", ", then "])
    return example(joiner.join(phrases), calls, pool)


def gen_near_miss(rng, real):
    """Instructions whose wording shadows a sibling tool's name."""
    pool = toolbox.TOOLS
    tricky = [
        ("What time is it in {tz}", "get_time", lambda: {"timezone": rng.choice(["UTC", "EAT", "JST", "BST"])}),
        ("Get the time for timezone {tz}", "get_time", lambda: {"timezone": rng.choice(["UTC", "EAT", "JST"])}),
        ("Read the note named {w}", "read_note", lambda: {"name": rng.choice(WORDS)}),
        ("Search the notes for {w}", "search_notes", lambda: {"query": rng.choice(WORDS + CITIES)}),
        ("List every timer", "list_timers", dict),
        ("What timers are set", "list_timers", dict),
    ]
    template, name, argfn = rng.choice(tricky)
    args = argfn()
    instruction = template.format(tz=args.get("timezone", ""), w=args.get("name", args.get("query", "")))
    return example(instruction, [(name, args)], pool)


def gen_exact_name(rng, real):
    """'my jetson note' -> name jetson; descriptor words never enter values."""
    pool = toolbox.TOOLS
    word = rng.choice(WORDS)
    templates = [
        ("Read my %s note" % word, "read_note", {"name": word}),
        ("Read the note called %s" % word, "read_note", {"name": word}),
        ("Open the %s note and read it" % word, "read_note", {"name": word}),
        ("Search my notes for %s" % word, "search_notes", {"query": word}),
    ]
    text, name, args = rng.choice(templates)
    return example(text, [(name, args)], pool)


def real_args(tool, rng):
    """Plausible arguments for the real toolbox."""
    makers = {
        "get_weather": lambda: {"city": rng.choice(CITIES)},
        "calculate": lambda: {"expression": "%d %s %d" % (
            rng.randint(2, 99999), rng.choice(["+", "-", "*", "/"]), rng.randint(2, 999))},
        "set_timer": lambda: dict(
            [("seconds", rng.choice([60, 90, 300, 600, 900, 1800, 5400, 7200,
                                     rng.randint(2, 99999)]))]
            + ([("label", rng.choice(WORDS))] if rng.random() < 0.8 else [])),
        "list_timers": dict,
        "get_time": lambda: {"timezone": rng.choice(["UTC", "EAT", "JST", "BST"])},
        "search_notes": lambda: {"query": rng.choice(WORDS + CITIES)},
        "read_note": lambda: {"name": rng.choice(WORDS)},
        "convert_units": lambda: {"value": rng.choice([2, 29, 340, 5, 100, rng.randint(1, 9999)]),
                                  "from_unit": rng.choice(["km", "mi", "kg", "lb", "c", "f", "m", "ft"]),
                                  "to_unit": rng.choice(["km", "mi", "kg", "lb", "c", "f", "m", "ft"])},
    }
    return makers[tool["name"]]()


GENERATORS = [
    (gen_single, 0.40),
    (gen_compound, 0.30),
    (gen_near_miss, 0.15),
    (gen_exact_name, 0.15),
]


def generate(n, rng):
    rows = []
    for _ in range(n):
        pick, acc = rng.random(), 0.0
        for gen, weight in GENERATORS:
            acc += weight
            if pick <= acc:
                rows.append(gen(rng, real=rng.random() < 0.5))
                break
    return rows


# --- the held-out eval: the verbatim measured failures ------------------

def eval_set():
    """Labelled picker-only cases. NEVER trained on. Includes the exact
    inputs that failed in the loop evals."""
    T = toolbox.TOOLS
    cases = [
        # register corruption, measured verbatim
        {"instruction": "set_timer(600, standup)",
         "expect": [["set_timer", {"seconds": 600}]]},
        {"instruction": "set_timer(seconds=600, label=standup)",
         "expect": [["set_timer", {"seconds": 600}]]},
        {"instruction": "Set a timer for 600 seconds labelled standup",
         "expect": [["set_timer", {"seconds": 600, "label": "standup"}]]},
        {"instruction": "Set a timer for 5400 seconds to check the oven",
         "expect": [["set_timer", {"seconds": 5400}]]},
        {"instruction": "Set a timer for 300 seconds labelled tea",
         "expect": [["set_timer", {"seconds": 300}]]},
        # the killer near-miss
        {"instruction": "Call get_time with timezone JST",
         "expect": [["get_time", {"timezone": "JST"}]]},
        {"instruction": "What time is it in Tokyo, timezone JST",
         "expect": [["get_time", {"timezone": "JST"}]]},
        # exact-name
        {"instruction": "Read my jetson note",
         "expect": [["read_note", {"name": "jetson"}]]},
        {"instruction": "Read the note named travel",
         "expect": [["read_note", {"name": "travel"}]]},
        # fan-out, incl. the intent-blend case
        {"instruction": "Convert 340 km to mi and calculate 18000 / 5",
         "expect": [["convert_units", {"value": 340, "from_unit": "km", "to_unit": "mi"}],
                     ["calculate", {"expression": "18000 / 5"}]]},
        {"instruction": "Get the weather for Nairobi and set a timer for 300 seconds labelled tea",
         "expect": [["get_weather", {"city": "Nairobi"}],
                     ["set_timer", {"seconds": 300, "label": "tea"}]]},
        {"instruction": "Get the weather for Nairobi, Kisumu and Mombasa",
         "expect": [["get_weather", {"city": "Nairobi"}],
                     ["get_weather", {"city": "Kisumu"}],
                     ["get_weather", {"city": "Mombasa"}]]},
        # plain singles as regression guards
        {"instruction": "Get the weather for Eldoret",
         "expect": [["get_weather", {"city": "Eldoret"}]]},
        {"instruction": "Calculate 1234 * 17",
         "expect": [["calculate", {"expression": "1234 * 17"}]]},
        {"instruction": "List every timer that is set",
         "expect": [["list_timers", {}]]},
        {"instruction": "Convert 29 c to f",
         "expect": [["convert_units", {"value": 29, "from_unit": "c", "to_unit": "f"}]]},
        # number fidelity, odd values
        {"instruction": "Set a timer for 4271 seconds labelled batch",
         "expect": [["set_timer", {"seconds": 4271}]]},
        {"instruction": "set_timer(7620, backup)",
         "expect": [["set_timer", {"seconds": 7620}]]},
    ]
    for case in cases:
        case["tools"] = "real"
    return cases


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--train", type=int, default=2000)
    parser.add_argument("--valid", type=int, default=120)
    parser.add_argument("--seed", type=int, default=7)
    opts = parser.parse_args()

    os.makedirs(OUT, exist_ok=True)
    rng = random.Random(opts.seed)
    train = generate(opts.train, rng)
    valid = generate(opts.valid, random.Random(opts.seed + 1))

    for name, rows in (("train", train), ("valid", valid)):
        path = os.path.join(OUT, "%s.jsonl" % name)
        with open(path, "w") as fh:
            for row in rows:
                fh.write(json.dumps(row) + "\n")
        print("%s: %d examples -> %s" % (name, len(rows), path))

    path = os.path.join(OUT, "picker_eval.jsonl")
    with open(path, "w") as fh:
        for case in eval_set():
            fh.write(json.dumps(case) + "\n")
    print("eval:  %d labelled cases -> %s (never trained on)" % (len(eval_set()), path))


if __name__ == "__main__":
    main()
