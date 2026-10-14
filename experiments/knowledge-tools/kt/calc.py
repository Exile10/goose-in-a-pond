"""Prototype of `calculate`'s in-process backend: exact arithmetic, percentages, units and date
differences from the question as the user asked it. No network, no key, no model. The Rust
implementation is meant to follow this grammar; this file exists to measure its coverage."""

import math
import re
from datetime import date
from fractions import Fraction

WORDS = [
    (r"\bmultiplied by\b|\btimes\b|\bx\b|×", "*"),
    (r"\bdivided by\b|\bover\b|÷", "/"),
    (r"\bplus\b|\badded to\b", "+"),
    (r"\bminus\b|\bless\b|−", "-"),
    (r"\bto the power of\b|\braised to\b|\*\*", "^"),
    (r"\bsquared\b", "^2"),
    (r"\bcubed\b", "^3"),
]

UNITS = {
    # length -> metres
    "mm": ("len", 0.001), "millimetre": ("len", 0.001), "millimeter": ("len", 0.001),
    "cm": ("len", 0.01), "centimetre": ("len", 0.01), "centimeter": ("len", 0.01),
    "m": ("len", 1.0), "metre": ("len", 1.0), "meter": ("len", 1.0),
    "km": ("len", 1000.0), "kilometre": ("len", 1000.0), "kilometer": ("len", 1000.0),
    "in": ("len", 0.0254), "inch": ("len", 0.0254), "ft": ("len", 0.3048), "foot": ("len", 0.3048),
    "feet": ("len", 0.3048), "yd": ("len", 0.9144), "yard": ("len", 0.9144),
    "mi": ("len", 1609.344), "mile": ("len", 1609.344),
    # mass -> kg
    "g": ("mass", 0.001), "gram": ("mass", 0.001), "kg": ("mass", 1.0), "kilogram": ("mass", 1.0),
    "lb": ("mass", 0.45359237), "pound": ("mass", 0.45359237), "oz": ("mass", 0.028349523125),
    "ounce": ("mass", 0.028349523125), "tonne": ("mass", 1000.0),
    # volume -> litres
    "ml": ("vol", 0.001), "millilitre": ("vol", 0.001), "l": ("vol", 1.0), "litre": ("vol", 1.0),
    "liter": ("vol", 1.0), "gallon": ("vol", 3.785411784), "cup": ("vol", 0.2365882365),
    # time -> seconds
    "second": ("time", 1.0), "sec": ("time", 1.0), "s": ("time", 1.0), "minute": ("time", 60.0),
    "min": ("time", 60.0), "hour": ("time", 3600.0), "hr": ("time", 3600.0), "h": ("time", 3600.0),
    "day": ("time", 86400.0), "week": ("time", 604800.0),
}
TEMPS = {"c": "c", "celsius": "c", "centigrade": "c", "f": "f", "fahrenheit": "f", "k": "k", "kelvin": "k"}
MONTHS = {m: i for i, m in enumerate(
    "january february march april may june july august september october november december".split(), 1)}


def _unit(word):
    w = word.lower().rstrip(".")
    for cand in (w, w[:-1] if w.endswith("s") else w, w[:-2] if w.endswith("es") else w):
        if cand in UNITS:
            return cand
    return None


def _num(s):
    return float(s.replace(",", ""))


def fmt(x):
    if isinstance(x, Fraction):
        if x.denominator == 1:
            return str(x.numerator)
        x = float(x)
    if abs(x - round(x)) < 1e-9 and abs(x) < 1e15:
        return str(int(round(x)))
    return ("%.6f" % x).rstrip("0").rstrip(".")


# ── expressions: a Pratt parser over exact fractions where it can ────────────


def _tokens(expr):
    return re.findall(r"\d+(?:\.\d+)?|sqrt|[-+*/^()%]", expr)


def evaluate(expr):
    toks = _tokens(expr)
    pos = [0]

    def peek():
        return toks[pos[0]] if pos[0] < len(toks) else None

    def take():
        t = peek()
        pos[0] += 1
        return t

    def atom():
        t = take()
        if t is None:
            raise ValueError("expression ends early")
        if t == "(":
            v = expr_(0)
            if take() != ")":
                raise ValueError("unbalanced parenthesis")
            return v
        if t == "-":
            return -atom()
        if t == "sqrt":
            v = atom()
            return Fraction(math.sqrt(v)) if v >= 0 else (_ for _ in ()).throw(ValueError("negative root"))
        if re.match(r"\d", t):
            return Fraction(t)
        raise ValueError("unexpected %r" % t)

    prec = {"+": 1, "-": 1, "*": 2, "/": 2, "^": 3}

    def expr_(minp):
        left = atom()
        while True:
            op = peek()
            if op == "%":
                take()
                left = left / 100
                continue
            if op not in prec or prec[op] < minp:
                return left
            take()
            right = expr_(prec[op] + (0 if op == "^" else 1))
            if op == "+":
                left += right
            elif op == "-":
                left -= right
            elif op == "*":
                left *= right
            elif op == "/":
                left /= right
            else:
                left = Fraction(float(left) ** float(right)) if right.denominator != 1 else left ** int(right)

    v = expr_(0)
    if pos[0] != len(toks):
        raise ValueError("left over: %s" % " ".join(toks[pos[0]:]))
    return v


# ── the question forms ───────────────────────────────────────────────────────

DATE = r"(\d{1,2})(?:st|nd|rd|th)?\s+(%s)\s+(\d{4})" % "|".join(MONTHS)


def _date(m):
    return date(int(m[2]), MONTHS[m[1]], int(m[0]))


def calculate(question, today=date(2026, 10, 14)):
    q = question.strip().rstrip("?.!").lower()
    q = q.replace("’", "'")

    dates = re.findall(DATE, q)
    if len(dates) == 2 and re.search(r"\bdays?\b", q):
        a, b = _date(dates[0]), _date(dates[1])
        return "From %s to %s is %d days" % (a.strftime("%-d %B %Y"), b.strftime("%-d %B %Y"), abs((b - a).days))
    if len(dates) == 1 and re.search(r"\b(since|until|till|ago|from today)\b", q):
        d = _date(dates[0])
        return "%s is %d days from today (%s)" % (d.strftime("%-d %B %Y"), abs((d - today).days), today.strftime("%-d %B %Y"))

    m = re.search(r"(-?[\d,.]+)\s*(?:degrees?\s*)?(celsius|centigrade|fahrenheit|kelvin|c|f|k)\b.*?\b(?:to|in|into)\s+"
                  r"(?:degrees?\s*)?(celsius|centigrade|fahrenheit|kelvin|c|f|k)\b", q)
    if m:
        v, a, b = _num(m[1]), TEMPS[m[2]], TEMPS[m[3]]
        c = v if a == "c" else (v - 32) * 5 / 9 if a == "f" else v - 273.15
        out = c if b == "c" else c * 9 / 5 + 32 if b == "f" else c + 273.15
        return "%s °%s = %s °%s" % (fmt(v), a.upper(), fmt(round(out, 2)), b.upper())

    # "how many X in N Y", "N Y in/to X"
    m = re.search(r"how many (\w+) (?:are )?(?:in|is) (-?[\d,.]+) (\w+)", q) or None
    if m:
        to_u, v, from_u = _unit(m[1]), _num(m[2]), _unit(m[3])
        if to_u and from_u and UNITS[to_u][0] == UNITS[from_u][0]:
            out = v * UNITS[from_u][1] / UNITS[to_u][1]
            return "%s %s = %s %s" % (fmt(v), m[3], fmt(round(out, 4)), m[1])
    m = re.search(r"(-?[\d,.]+)\s*(\w+)\s+(?:to|in|into)\s+(\w+)", q)
    if m and _unit(m[2]) and _unit(m[3]) and UNITS[_unit(m[2])][0] == UNITS[_unit(m[3])][0]:
        v = _num(m[1])
        out = v * UNITS[_unit(m[2])][1] / UNITS[_unit(m[3])][1]
        return "%s %s = %s %s" % (fmt(v), m[2], fmt(round(out, 4)), m[3])

    # percentages: "15 percent of 2,350", "16% of 48000"
    m = re.search(r"(-?[\d,.]+)\s*(?:%|percent|per cent)\s+of\s+(-?[\d,.]+)", q)
    if m:
        p, base = Fraction(m[1].replace(",", "")), Fraction(m[2].replace(",", ""))
        return "%s%% of %s = %s" % (fmt(p), fmt(base), fmt(p * base / 100))

    # "save 2,500 every week ... after 52 weeks" -- rate times count
    m = re.search(r"([\d,.]+)\s*(?:\w+\s+)?(?:every|each|a|per)\s+(day|week|month|year).*?(\d[\d,]*)\s+\2s?", q)
    if m:
        a, n = Fraction(m[1].replace(",", "")), Fraction(m[3].replace(",", ""))
        return "%s × %s = %s" % (fmt(a), fmt(n), fmt(a * n))

    # square root, then a plain expression in words or symbols
    e = q
    e = re.sub(r"square root of\s*", "sqrt ", e)
    for pat, rep in WORDS:
        e = re.sub(pat, rep, e)
    e = re.sub(r"(?<=\d),(?=\d{3})", "", e)
    e = re.sub(r"to (\w+) decimal places?|to \d+ dp", "", e)
    expr = "".join(re.findall(r"\d+(?:\.\d+)?|sqrt|[-+*/^()%]", e))
    if re.search(r"\d", expr):
        try:
            v = evaluate(" ".join(_tokens(e)))
            shown = " ".join(_tokens(e)).replace("*", "×").replace(" ( ", " (").replace(" ) ", ") ")
            places = re.search(r"to (\w+) decimal places?", q)
            if places:
                n = {"one": 1, "two": 2, "three": 3, "four": 4, "five": 5, "six": 6}.get(places[1], None)
                n = n if n is not None else int(places[1]) if places[1].isdigit() else 6
                return "%s ≈ %.*f" % (shown, n, float(v))
            return "%s = %s" % (shown, fmt(v))
        except (ValueError, ZeroDivisionError) as exc:
            return None if "unexpected" in str(exc) else None
    return None
