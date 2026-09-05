#!/usr/bin/env python3
"""Parser and toolbox checks. No servers needed: python3 test_parsing.py"""

import sys

from dualgemma import agent, fgemma, tools

FAILURES = []


def check(label, got, want):
    if got != want:
        FAILURES.append("%s\n     got  %r\n     want %r" % (label, got, want))


# --- FunctionGemma output parsing --------------------------------------

check("string arg",
      fgemma.parse_call("<start_function_call>call:get_weather{city:<escape>Nairobi<escape>}<end_function_call>"),
      ("get_weather", {"city": "Nairobi"}))

check("int and string args",
      fgemma.parse_call("<start_function_call>call:set_timer{seconds:600,label:<escape>standup<escape>}"),
      ("set_timer", {"seconds": 600, "label": "standup"}))

check("float arg",
      fgemma.parse_call("<start_function_call>call:convert_units{value:2.5,from_unit:<escape>kg<escape>,to_unit:<escape>lb<escape>}"),
      ("convert_units", {"value": 2.5, "from_unit": "kg", "to_unit": "lb"}))

check("zero-arg call",
      fgemma.parse_call("<start_function_call>call:list_timers{}<end_function_call>"),
      ("list_timers", {}))

check("escaped value containing a comma",
      fgemma.parse_call("<start_function_call>call:calculate{expression:<escape>1,234 * 17<escape>}"),
      ("calculate", {"expression": "1,234 * 17"}))

# It never stops -- only the first call may ever be read.
runaway = ("<start_function_call>call:calculate{expression:<escape>18000 / 5<escape>}<end_function_call>"
           "call:get_weather{city:<escape>Tokyo<escape>}<end_function_call>")
check("runaway output takes only the first call",
      fgemma.parse_call(runaway), ("calculate", {"expression": "18000 / 5"}))

name, reason = fgemma.parse_call("I apologize, but I cannot assist with setting timers.")
check("refusal yields no call", name, None)
check("refusal explains itself", reason.startswith("no function call"), True)

# --- multi-call head-run parsing ---------------------------------------

compound = ("<start_function_call>call:get_weather{city:<escape>Nairobi<escape>}<end_function_call>"
            "<start_function_call>call:set_timer{seconds:300,label:<escape>Tea<escape>}<end_function_call>"
            "call:list_timers{required:[]}<end_function_call>call:set_timer{seconds:0}")
check("compound keeps the tagged head-run",
      fgemma.parse_calls(compound),
      ([("get_weather", {"city": "Nairobi"}), ("set_timer", {"seconds": 300, "label": "Tea"})], None))

three = ("<start_function_call>call:get_weather{city:<escape>Nairobi<escape>}<end_function_call>"
         "<start_function_call>call:get_weather{city:<escape>Kisumu<escape>}<end_function_call>"
         "<start_function_call>call:get_weather{city:<escape> Mombasa<escape>}<end_function_call>"
         "call:get_weather{city:<escape>Nairobi<escape>}<end_function_call>"
         "<start_function_call>call:get_weather{city:<escape> Mombasa<escape>}")
check("three tagged calls survive, tail garbage does not (values stripped)",
      fgemma.parse_calls(three),
      ([("get_weather", {"city": "Nairobi"}), ("get_weather", {"city": "Kisumu"}),
        ("get_weather", {"city": "Mombasa"})], None))

single_run = ("<start_function_call>call:set_timer{seconds:600,label:<escape>standup<escape>}<end_function_call>"
              "call:search_notes{query:<escape>standup<escape>}")
check("single-intent output yields exactly one call",
      fgemma.parse_calls(single_run),
      ([("set_timer", {"seconds": 600, "label": "standup"})], None))

calls, err = fgemma.parse_calls("I cannot assist with that request.")
check("refusal yields no calls", calls, [])
check("refusal reason is explained", err.startswith("no function call"), True)

dupes = ("<start_function_call>call:get_weather{city:<escape>Nairobi<escape>}<end_function_call>"
         "<start_function_call>call:get_weather{city:<escape>Nairobi<escape>}<end_function_call>")
check("exact tagged repeats are deduped",
      fgemma.parse_calls(dupes),
      ([("get_weather", {"city": "Nairobi"})], None))

check("ASK directive extraction",
      agent._first_directive("ASK: Get the weather for Nairobi and set a timer for 300 seconds", ("ASK", "ANSWER")),
      ("ASK", "Get the weather for Nairobi and set a timer for 300 seconds"))

# --- declaration rendering ---------------------------------------------

decl = fgemma.declaration(tools.BY_NAME["set_timer"])
check("declaration names the tool", "declaration:set_timer{" in decl, True)
check("declaration marks INTEGER", "type:<escape>INTEGER<escape>" in decl, True)
check("declaration lists required", "required:[<escape>seconds<escape>]" in decl, True)
check("optional arg stays out of required", "<escape>label<escape>]" in decl, False)

check("zero-arg declaration renders", "properties:{},required:[]" in
      fgemma.declaration(tools.BY_NAME["list_timers"]), True)

# --- planner directive extraction --------------------------------------

check("plain TOOL line",
      agent._first_directive("TOOL: Call get_weather for the city Nairobi", ("TOOL", "ANSWER")),
      ("TOOL", "Call get_weather for the city Nairobi"))

check("preamble before the directive",
      agent._first_directive("Sure, I can help.\nANSWER: It is 21 degrees.", ("TOOL", "ANSWER")),
      ("ANSWER", "It is 21 degrees."))

check("fenced and bulleted",
      agent._first_directive("```\n- TOOL: Call list_timers\n```", ("TOOL", "ANSWER")),
      ("TOOL", "Call list_timers"))

check("bare prose falls through to an answer",
      agent._first_directive("It is 21 degrees.", ("TOOL", "ANSWER"))[0], None)

check("solo JSON call",
      agent._parse_solo_call('{"tool": "set_timer", "args": {"seconds": 600}}'),
      ("set_timer", {"seconds": 600}, None))

check("solo call naming an unknown tool is rejected",
      agent._parse_solo_call('{"tool": "order_pizza", "args": {}}')[2],
      "no tool named 'order_pizza'")

# --- tool execution -----------------------------------------------------

tools.reset()
check("calculate", tools.execute("calculate", {"expression": "18000 / 5"}), "18000 / 5 = 3600")
check("calculate rejects code", tools.execute("calculate", {"expression": "__import__('os')"}).startswith("Could not evaluate"), True)
check("convert_units", tools.execute("convert_units", {"value": 340, "from_unit": "km", "to_unit": "mi"}), "340 km = 211.266 mi")
check("set_timer", tools.execute("set_timer", {"seconds": 600, "label": "standup"}), "Timer set: standup in 600 seconds (10 minutes).")
check("list_timers sees it", tools.execute("list_timers", {}), "standup in 600 seconds")
check("missing required arg", tools.execute("set_timer", {"label": "tea"}), "TOOL_ERROR: set_timer is missing required argument(s): seconds")
check("unknown arg is dropped, not fatal", tools.execute("get_weather", {"city": "Nairobi", "units": "c"}).startswith("Nairobi: 21"), True)
check("unknown tool", tools.execute("order_pizza", {}), "TOOL_ERROR: no tool named 'order_pizza'")
check("unknown city is reported, not invented", tools.execute("get_weather", {"city": "Atlantis"}).startswith("No weather station"), True)
check("read_note", "09:15 EAT" in tools.execute("read_note", {"name": "standup"}), True)
check("search_notes", "jetson" in tools.execute("search_notes", {"query": "Orin"}).lower(), True)
tools.reset()
check("reset clears timers", tools.execute("list_timers", {}), "No timers set.")

# --- report -------------------------------------------------------------

if FAILURES:
    print("%d FAILED" % len(FAILURES))
    for failure in FAILURES:
        print("  -- " + failure)
    sys.exit(1)
print("all parsing and toolbox checks passed")
