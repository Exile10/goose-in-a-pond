"""A small, deterministic, offline toolbox.

Every tool is real code over a local sandbox: no network, no clock
dependence except `get_time`, so an eval run is reproducible. Weather is
a fixed lookup table and says so in its output -- it is a stand-in for a
real provider, not a claim about the weather.

Schemas are declared once here and rendered two ways: as FunctionGemma
declarations (dualgemma.fgemma) and as prose for the planner.

Parameter descriptions stay terse because FunctionGemma copies them
verbatim into argument VALUES -- a description reading "Unit to convert
from: km mi kg lb c f m ft" came back as `from_unit: "km mi kg lb c f m
ft"`. Anything the planner needs but the transducer must not echo goes
in `hint`, which only `prose_catalogue` renders.
"""

import ast
import datetime
import operator
import os

SANDBOX = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "sandbox")
NOTES = os.path.join(SANDBOX, "notes")

_TIMERS = []

_WEATHER = {
    "nairobi": (21, "light cloud"),
    "kisumu": (29, "humid and clear"),
    "mombasa": (31, "hot, sea breeze"),
    "eldoret": (17, "cool drizzle"),
    "london": (11, "overcast"),
    "tokyo": (24, "clear"),
}

_UNITS = {
    ("km", "mi"): lambda v: v * 0.621371,
    ("mi", "km"): lambda v: v / 0.621371,
    ("kg", "lb"): lambda v: v * 2.20462,
    ("lb", "kg"): lambda v: v / 2.20462,
    ("c", "f"): lambda v: v * 9 / 5 + 32,
    ("f", "c"): lambda v: (v - 32) * 5 / 9,
    ("m", "ft"): lambda v: v * 3.28084,
    ("ft", "m"): lambda v: v / 3.28084,
}

_MATH_OPS = {
    ast.Add: operator.add,
    ast.Sub: operator.sub,
    ast.Mult: operator.mul,
    ast.Div: operator.truediv,
    ast.Pow: operator.pow,
    ast.Mod: operator.mod,
    ast.FloorDiv: operator.floordiv,
    ast.USub: operator.neg,
    ast.UAdd: operator.pos,
}


def _eval(node):
    if isinstance(node, ast.Expression):
        return _eval(node.body)
    if isinstance(node, ast.Constant) and isinstance(node.value, (int, float)):
        return node.value
    if isinstance(node, ast.BinOp) and type(node.op) in _MATH_OPS:
        return _MATH_OPS[type(node.op)](_eval(node.left), _eval(node.right))
    if isinstance(node, ast.UnaryOp) and type(node.op) in _MATH_OPS:
        return _MATH_OPS[type(node.op)](_eval(node.operand))
    raise ValueError("unsupported expression")


# --- implementations ---------------------------------------------------

def get_weather(city):
    key = str(city).strip().lower()
    if key not in _WEATHER:
        return "No weather station for %r. Known cities: %s." % (
            city, ", ".join(sorted(_WEATHER)))
    temp, sky = _WEATHER[key]
    return "%s: %d degrees C, %s. (synthetic fixture, not live data)" % (
        str(city).title(), temp, sky)


def calculate(expression):
    try:
        value = _eval(ast.parse(str(expression), mode="eval"))
    except Exception as exc:
        return "Could not evaluate %r: %s" % (expression, exc)
    if isinstance(value, float) and value.is_integer():
        value = int(value)
    if isinstance(value, float):
        value = round(value, 6)
    return "%s = %s" % (expression, value)


def set_timer(seconds, label=None):
    try:
        seconds = int(seconds)
    except (TypeError, ValueError):
        return "seconds must be a whole number of seconds, got %r" % (seconds,)
    if seconds <= 0:
        return "seconds must be positive, got %d" % seconds
    label = label or "timer"
    _TIMERS.append({"seconds": seconds, "label": label})
    return "Timer set: %s in %d seconds (%s)." % (
        label, seconds, _humanise(seconds))


def list_timers():
    if not _TIMERS:
        return "No timers set."
    return "; ".join(
        "%s in %d seconds" % (t["label"], t["seconds"]) for t in _TIMERS)


def get_time(timezone="UTC"):
    now = datetime.datetime.now(datetime.timezone.utc)
    offsets = {"utc": 0, "eat": 3, "nairobi": 3, "bst": 1, "london": 1, "jst": 9, "tokyo": 9}
    hours = offsets.get(str(timezone).strip().lower())
    if hours is None:
        return "Unknown timezone %r. Known: %s." % (timezone, ", ".join(sorted(offsets)))
    local = now + datetime.timedelta(hours=hours)
    return "%s: %s" % (str(timezone).upper(), local.strftime("%Y-%m-%d %H:%M"))


def search_notes(query):
    hits = []
    needle = str(query).strip().lower()
    for name in sorted(os.listdir(NOTES)):
        if not name.endswith(".md"):
            continue
        with open(os.path.join(NOTES, name)) as handle:
            body = handle.read()
        if needle in body.lower() or needle in name.lower():
            first = body.strip().splitlines()[0] if body.strip() else ""
            hits.append("%s -- %s" % (name[:-3], first))
    if not hits:
        return "No notes match %r." % query
    return "Matching notes: " + "; ".join(hits)


def read_note(name):
    stem = str(name).strip().replace(".md", "")
    path = os.path.join(NOTES, stem + ".md")
    if not os.path.isfile(path):
        available = ", ".join(sorted(n[:-3] for n in os.listdir(NOTES) if n.endswith(".md")))
        return "No note named %r. Available: %s." % (stem, available)
    with open(path) as handle:
        return "Note %s:\n%s" % (stem, handle.read().strip())


def convert_units(value, from_unit, to_unit):
    try:
        value = float(value)
    except (TypeError, ValueError):
        return "value must be a number, got %r" % (value,)
    pair = (str(from_unit).strip().lower(), str(to_unit).strip().lower())
    if pair not in _UNITS:
        return "Cannot convert %s to %s. Known pairs: %s." % (
            from_unit, to_unit, ", ".join("%s->%s" % p for p in _UNITS))
    result = round(_UNITS[pair](value), 4)
    return "%g %s = %g %s" % (value, pair[0], result, pair[1])


def _humanise(seconds):
    if seconds % 3600 == 0:
        return "%d hours" % (seconds // 3600)
    if seconds % 60 == 0:
        return "%d minutes" % (seconds // 60)
    return "%d seconds" % seconds


# --- schemas -----------------------------------------------------------

TOOLS = [
    {
        "name": "get_weather",
        "description": "Get the current weather for a named city",
        "parameters": {"city": {"type": "string", "description": "City name"}},
        "required": ["city"],
        "fn": get_weather,
    },
    {
        "name": "calculate",
        "description": "Evaluate a numeric arithmetic expression",
        "parameters": {
            "expression": {
                "type": "string",
                "description": "Arithmetic expression",
                "hint": "digits and + - * / ** % only",
            }
        },
        "required": ["expression"],
        "fn": calculate,
    },
    {
        "name": "set_timer",
        "description": "Set a countdown timer for a whole number of seconds",
        "parameters": {
            "seconds": {"type": "integer", "description": "Duration in seconds"},
            "label": {"type": "string", "description": "What the timer is for"},
        },
        "required": ["seconds"],
        "fn": set_timer,
    },
    {
        "name": "list_timers",
        "description": "List every timer that is currently set",
        "parameters": {},
        "required": [],
        "fn": list_timers,
    },
    {
        "name": "get_time",
        "description": "Get the current clock time in a timezone",
        "parameters": {
            "timezone": {"type": "string", "description": "Timezone code",
                         "hint": "one of UTC, EAT, BST, JST"}
        },
        "required": ["timezone"],
        "fn": get_time,
    },
    {
        "name": "search_notes",
        "description": "Search the user's saved notes for a keyword",
        "parameters": {"query": {"type": "string", "description": "Keyword to search for"}},
        "required": ["query"],
        "fn": search_notes,
    },
    {
        "name": "read_note",
        "description": "Read one saved note in full by its name",
        "parameters": {"name": {"type": "string", "description": "Name of the note"}},
        "required": ["name"],
        "fn": read_note,
    },
    {
        "name": "convert_units",
        "description": "Convert a value between units of distance, mass or temperature",
        "parameters": {
            "value": {"type": "number", "description": "The number to convert"},
            "from_unit": {"type": "string", "description": "Unit to convert from",
                          "hint": "one of km, mi, kg, lb, c, f, m, ft"},
            "to_unit": {"type": "string", "description": "Unit to convert to",
                        "hint": "one of km, mi, kg, lb, c, f, m, ft"},
        },
        "required": ["value", "from_unit", "to_unit"],
        "fn": convert_units,
    },
]

BY_NAME = {t["name"]: t for t in TOOLS}


def reset():
    """Clear mutable tool state between eval cases."""
    del _TIMERS[:]


def execute(name, args):
    tool = BY_NAME.get(name)
    if tool is None:
        return "TOOL_ERROR: no tool named %r" % name
    args = dict(args or {})
    accepted = set(tool["parameters"])
    unknown = [k for k in args if k not in accepted]
    for key in unknown:
        args.pop(key)
    missing = [r for r in tool["required"] if r not in args]
    if missing:
        return "TOOL_ERROR: %s is missing required argument(s): %s" % (
            name, ", ".join(missing))
    try:
        return tool["fn"](**args)
    except Exception as exc:
        return "TOOL_ERROR: %s failed: %s" % (name, exc)


def prose_catalogue():
    """The tool list as the planner sees it."""
    lines = []
    for tool in TOOLS:
        if tool["parameters"]:
            rendered = []
            for key, spec in tool["parameters"].items():
                if spec.get("hint"):
                    rendered.append("%s (%s, %s)" % (key, spec["type"], spec["hint"]))
                else:
                    rendered.append("%s (%s)" % (key, spec["type"]))
            params = ", ".join(rendered)
        else:
            params = "no arguments"
        required = ", ".join(tool["required"]) or "none"
        lines.append(
            "- %s(%s) -- %s. Required: %s."
            % (tool["name"], params, tool["description"], required))
    return "\n".join(lines)
