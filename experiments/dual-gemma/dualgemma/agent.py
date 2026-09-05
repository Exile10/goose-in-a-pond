"""The two arms under test.

DUAL   user -> E2B -> FunctionGemma -> tool -> E2B -> ... -> user
SOLO   user -> E2B (emitting its own JSON calls) -> tool -> E2B -> user

Both arms see the same tools, run the same loop, execute through the same
`tools.execute`, and get the same step budget. The only variable is who
writes the call.
"""

import json
import re

from . import fgemma, tools


def named_tool(intent, catalogue=None):
    """Which tool did the planner actually name in its instruction?

    Cooperation is only measurable when the planner was explicit. If the
    instruction names exactly one tool, the picker either honoured it or
    overruled it, and that is a fact we can count.
    """
    catalogue = catalogue or tools.TOOLS
    lowered = (intent or "").lower()
    found = [(lowered.find(t["name"]), t["name"])
             for t in catalogue if t["name"] in lowered]
    if not found:
        return None
    found.sort()
    return found[0][1]

MAX_STEPS = 4

# A mis-pick is not an error, so nothing in the loop notices it: the call
# succeeds, returns something useless, and at temperature 0 the planner
# reproduces the same instruction forever. Both arms get the same guard.
_REPEAT_NUDGE = (
    " (NOTE: that exact instruction has now produced this same result twice."
    " It is not working. Name a different tool, phrase it differently, or"
    " answer without a tool.)"
)

_DUAL_SYSTEM = """You are the reasoning half of a two-model assistant.

You do the thinking. A second, much smaller model called FunctionGemma \
does the tool calling. FunctionGemma cannot reason, cannot do arithmetic, \
and cannot resolve anything you leave implicit -- it only turns a literal \
instruction into a function call.

Tools FunctionGemma can call:
{catalogue}

Reply with exactly one line, and nothing else. It must start with TOOL: or ANSWER:.

TOOL: <one plain imperative naming exactly one tool and spelling out every argument>
    Resolve units, arithmetic, dates and references YOURSELF before writing this line.
    Good: TOOL: Call set_timer with seconds 600 and label standup
    Bad:  TOOL: Set a timer for ten minutes
    Good: TOOL: Call convert_units with value 340, from_unit km, to_unit mi
    Bad:  TOOL: Convert that distance to miles

ANSWER: <your reply to the user>
    Use this the moment you can answer, including when no tool is needed.
    Never invent a tool result; only report what an OBSERVATION gave you.

You will see the result of each call as an OBSERVATION line. If an \
OBSERVATION reports an error, rewrite the TOOL line more literally, or \
answer without it."""

_NATURAL_SYSTEM = """You are the reasoning half of a two-model assistant.

You have exactly one tool: a tiny helper model that operates gadgets for \
you. You speak to it in plain natural language and it translates your \
request into function calls. It can bundle SEVERAL actions from one \
request. It cannot reason at all: it cannot do arithmetic, convert units, \
resolve dates, or work out what "that" refers to. Everything in your \
request must already be literal.

What the helper's gadgets can do:
{catalogue}

Reply with exactly one line, and nothing else. It must start with ASK: or ANSWER:.

ASK: <a plain request to the helper, one or several actions>
    Resolve every number, unit and reference YOURSELF first.
    Write it as an ENGLISH SENTENCE. Never use function syntax, parentheses,
    quotes or equals signs -- the helper misreads code and corrupts the
    numbers. Keep numbers as digits, and use the exact unit codes and note
    names from the gadget list (km, not kilometers).
    Good: ASK: Get the weather for Nairobi and set a timer for 300 seconds labelled tea
    Good: ASK: Convert 340 km to mi and calculate 18000 / 5
    Bad:  ASK: get_weather(city=Nairobi) and set_timer(seconds=300, label=tea)
    Bad:  ASK: Check the weather where I am and set a tea timer for five minutes

ANSWER: <your reply to the user>
    Use this the moment you can answer, including when no gadget is needed.
    Never invent a result; only report what an OBSERVATION gave you.

Results come back as numbered OBSERVATION lines, one per action. If an \
OBSERVATION reports an error, rephrase more literally, or answer without it."""

_SOLO_SYSTEM = """You are a helpful assistant that can call tools.

Tools available:
{catalogue}

Reply with exactly one line, and nothing else. It must start with CALL: or ANSWER:.

CALL: {{"tool": "<name>", "args": {{"<key>": <value>}}}}
    A single line of JSON. Resolve units, arithmetic, dates and references
    yourself so the argument values are literal.
    Good: CALL: {{"tool": "set_timer", "args": {{"seconds": 600, "label": "standup"}}}}

ANSWER: <your reply to the user>
    Use this the moment you can answer, including when no tool is needed.
    Never invent a tool result; only report what an OBSERVATION gave you.

You will see the result of each call as an OBSERVATION line. If an \
OBSERVATION reports an error, correct the call, or answer without it."""


def _system(template):
    return template.format(catalogue=tools.prose_catalogue())


def _first_directive(text, verbs):
    """Pull the first TOOL:/ANSWER:/CALL: line out of a model reply."""
    cleaned = re.sub(r"```[a-z]*\n?", "", text or "").strip()
    for line in cleaned.splitlines():
        line = line.strip().lstrip("*- ")
        for verb in verbs:
            if line.upper().startswith(verb + ":"):
                return verb, line[len(verb) + 1:].strip()
    return None, cleaned


class Result:
    def __init__(self, arm):
        self.arm = arm
        self.answer = ""
        self.steps = []
        self.calls = []
        self.errors = []
        self.handoffs = []

    @property
    def agreement(self):
        """(honoured, measurable) hand-offs where the planner named a tool."""
        measurable = [h for h in self.handoffs if h["named"]]
        return sum(1 for h in measurable if h["agreed"]), len(measurable)

    @property
    def first_call(self):
        return self.calls[0] if self.calls else None

    def as_dict(self):
        return {
            "arm": self.arm,
            "answer": self.answer,
            "calls": self.calls,
            "errors": self.errors,
            "steps": self.steps,
            "handoffs": self.handoffs,
        }


def run_dual(planner, picker, question, max_steps=MAX_STEPS, trace=None):
    """E2B plans in prose; FunctionGemma turns each plan into a call."""
    result = Result("dual")
    history = [
        {"role": "system", "content": _system(_DUAL_SYSTEM)},
        {"role": "user", "content": question},
    ]
    previous = None

    for _ in range(max_steps):
        reply = planner.chat(history, max_tokens=1024)
        verb, payload = _first_directive(reply, ("TOOL", "ANSWER"))

        if verb == "ANSWER" or verb is None:
            result.answer = payload
            result.steps.append({"who": "planner", "kind": "answer", "text": payload})
            if trace:
                trace("planner", "answer", payload)
            return result

        result.steps.append({"who": "planner", "kind": "intent", "text": payload})
        if trace:
            trace("planner", "intent", payload)

        name, args, raw, error = fgemma.select(picker, payload, tools.TOOLS)
        wanted = named_tool(payload)
        result.handoffs.append({
            "intent": payload,
            "named": wanted,
            "picked": name,
            "agreed": None if not wanted else (name == wanted),
        })
        if error:
            result.errors.append(error)
            observation = "TOOL_ERROR: the tool caller could not act on that instruction (%s). Rewrite it more literally, naming the tool and every argument value." % error
            result.steps.append({"who": "picker", "kind": "error", "text": error, "raw": raw})
            if trace:
                trace("picker", "error", "%s | raw=%r" % (error, raw[:120]))
        else:
            result.calls.append({"tool": name, "args": args})
            result.steps.append({"who": "picker", "kind": "call", "tool": name, "args": args})
            if trace:
                trace("picker", "call", "%s(%s)" % (name, json.dumps(args)))
            observation = tools.execute(name, args)
            result.steps.append({"who": "tool", "kind": "observation", "text": observation})
            if trace:
                trace("tool", "observation", observation)

        if payload == previous:
            observation += _REPEAT_NUDGE
        previous = payload

        history.append({"role": "assistant", "content": "TOOL: " + payload})
        history.append({"role": "user", "content": "OBSERVATION: " + observation})

    result.answer = _final_sweep(planner, history, result, trace)
    return result


def run_natural(planner, picker, question, max_steps=MAX_STEPS, trace=None):
    """E2B talks to FunctionGemma in natural language; the picker may fan
    one ASK out into several calls. Thinking stays ON for the planner --
    the whole bet is that deliberation plus a forgiving interface beats
    deliberation plus a rigid one."""
    result = Result("natural")
    history = [
        {"role": "system", "content": _system(_NATURAL_SYSTEM)},
        {"role": "user", "content": question},
    ]
    previous = None

    for _ in range(max_steps):
        reply = planner.chat(history, max_tokens=1024)
        verb, payload = _first_directive(reply, ("ASK", "ANSWER"))

        if verb == "ANSWER" or verb is None:
            result.answer = payload
            result.steps.append({"who": "planner", "kind": "answer", "text": payload})
            if trace:
                trace("planner", "answer", payload)
            return result

        result.steps.append({"who": "planner", "kind": "intent", "text": payload})
        if trace:
            trace("planner", "intent", payload)

        calls, raw, error = fgemma.select_many(picker, payload, tools.TOOLS)
        # A compound ASK can legitimately name several tools; a call is
        # honoured if it matches ANY of them. (named_tool alone would score
        # the second and third calls of a fan-out as overruled.)
        lowered = payload.lower()
        wanted_set = {t["name"] for t in tools.TOOLS if t["name"] in lowered}
        wanted = "|".join(sorted(wanted_set)) if wanted_set else None
        if error:
            result.errors.append(error)
            result.handoffs.append({"intent": payload, "named": wanted,
                                    "picked": None, "agreed": None})
            observation = ("OBSERVATION 1: TOOL_ERROR: the helper could not act on that "
                           "request (%s). Rephrase it more literally." % error)
            result.steps.append({"who": "picker", "kind": "error", "text": error, "raw": raw})
            if trace:
                trace("picker", "error", "%s | raw=%r" % (error, raw[:120]))
        else:
            observations = []
            for index, (name, args) in enumerate(calls, 1):
                result.calls.append({"tool": name, "args": args})
                result.handoffs.append({
                    "intent": payload, "named": wanted, "picked": name,
                    "agreed": None if not wanted_set else (name in wanted_set),
                })
                result.steps.append({"who": "picker", "kind": "call",
                                     "tool": name, "args": args})
                if trace:
                    trace("picker", "call", "%s(%s)" % (name, json.dumps(args)))
                outcome = tools.execute(name, args)
                observations.append("OBSERVATION %d: %s" % (index, outcome))
                result.steps.append({"who": "tool", "kind": "observation", "text": outcome})
                if trace:
                    trace("tool", "observation", outcome)
            observation = "\n".join(observations)

        if payload == previous:
            observation += _REPEAT_NUDGE
        previous = payload

        history.append({"role": "assistant", "content": "ASK: " + payload})
        history.append({"role": "user", "content": observation})

    result.answer = _final_sweep(planner, history, result, trace)
    return result


def run_solo(planner, question, max_steps=MAX_STEPS, trace=None):
    """E2B alone: it writes its own JSON calls. The control arm."""
    result = Result("solo")
    history = [
        {"role": "system", "content": _system(_SOLO_SYSTEM)},
        {"role": "user", "content": question},
    ]
    previous = None

    for _ in range(max_steps):
        reply = planner.chat(history, max_tokens=1024)
        verb, payload = _first_directive(reply, ("CALL", "ANSWER"))

        if verb == "ANSWER" or verb is None:
            result.answer = payload
            result.steps.append({"who": "planner", "kind": "answer", "text": payload})
            if trace:
                trace("planner", "answer", payload)
            return result

        name, args, error = _parse_solo_call(payload)
        if error:
            result.errors.append(error)
            observation = "TOOL_ERROR: %s. Emit one line of valid JSON after CALL:." % error
            result.steps.append({"who": "planner", "kind": "error", "text": error})
            if trace:
                trace("planner", "error", "%s | raw=%r" % (error, payload[:120]))
        else:
            result.calls.append({"tool": name, "args": args})
            result.steps.append({"who": "planner", "kind": "call", "tool": name, "args": args})
            if trace:
                trace("planner", "call", "%s(%s)" % (name, json.dumps(args)))
            observation = tools.execute(name, args)
            result.steps.append({"who": "tool", "kind": "observation", "text": observation})
            if trace:
                trace("tool", "observation", observation)

        if payload == previous:
            observation += _REPEAT_NUDGE
        previous = payload

        history.append({"role": "assistant", "content": "CALL: " + payload})
        history.append({"role": "user", "content": "OBSERVATION: " + observation})

    result.answer = _final_sweep(planner, history, result, trace)
    return result


def _parse_solo_call(payload):
    start, end = payload.find("{"), payload.rfind("}")
    if start == -1 or end <= start:
        return None, None, "no JSON object found"
    try:
        blob = json.loads(payload[start:end + 1])
    except ValueError as exc:
        return None, None, "invalid JSON (%s)" % exc
    name = blob.get("tool") or blob.get("name")
    if not name:
        return None, None, "JSON has no 'tool' field"
    if name not in tools.BY_NAME:
        return None, None, "no tool named %r" % name
    args = blob.get("args", blob.get("arguments", {}))
    if not isinstance(args, dict):
        return None, None, "'args' must be an object"
    return name, args, None


def _final_sweep(planner, history, result, trace):
    """Step budget exhausted: make the model answer from what it has."""
    history.append({
        "role": "user",
        "content": "You have run out of tool calls. Reply now with a single ANSWER: line using only the observations above.",
    })
    reply = planner.chat(history, max_tokens=1024)
    _, payload = _first_directive(reply, ("ANSWER",))
    result.steps.append({"who": "planner", "kind": "answer", "text": payload})
    if trace:
        trace("planner", "answer", payload)
    return payload
