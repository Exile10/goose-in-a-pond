"""FunctionGemma: prompt construction and output parsing.

FunctionGemma does not speak JSON tool-calling. It has its own token
grammar, and it was trained with the declarations inlined into a
`developer` turn. Ported from the Rust implementation in
`crates/pond-adapters-local-inference/src/tool_caller.rs`, extended here
to declare several tools at once so the model *picks* rather than only
filling arguments for a tool someone else already chose.

Two behaviours matter and are handled below:

1. It does not stop. After the first well-formed call it keeps emitting
   plausible-looking garbage forever, so `<end_function_call>` is a hard
   stop sequence and only the first call is ever read.
2. It does not reason. Give it "wake me in 10 minutes" and it refuses;
   give it "seconds 600" and it is perfect. Normalising the request is
   the planner's job, not its own.
"""

import re

START_CALL = "<start_function_call>call:"
END_CALL = "<end_function_call>"
STOPS = [END_CALL, "<end_of_turn>", "<start_of_turn>"]
MULTI_STOPS = ["<end_of_turn>", "<start_of_turn>"]


def declaration(tool):
    """Render one tool as a FunctionGemma declaration.

    Format, all on one line and with no spaces around the punctuation:
      declaration:NAME{description:<escape>D<escape>,parameters:{properties:{
      key:{description:<escape>D<escape>,type:<escape>TYPE<escape>}},
      required:[<escape>key<escape>],type:<escape>OBJECT<escape>}}
    """
    props = tool.get("parameters", {})
    required = tool.get("required", [])

    rendered = []
    for name, spec in props.items():
        rendered.append(
            "%s:{description:<escape>%s<escape>,type:<escape>%s<escape>}"
            % (name, spec["description"], spec["type"].upper())
        )
    required_block = ",".join("<escape>%s<escape>" % r for r in required)

    return (
        "<start_function_declaration>declaration:%s{"
        "description:<escape>%s<escape>,"
        "parameters:{properties:{%s},required:[%s],type:<escape>OBJECT<escape>}}"
        "<end_function_declaration>"
        % (tool["name"], tool["description"], ",".join(rendered), required_block)
    )


def build_prompt(instruction, tools):
    """The developer turn carries every declaration; the user turn carries
    the planner's already-normalised instruction."""
    decls = "".join(declaration(t) for t in tools)
    return (
        "<start_of_turn>developer\n"
        "You are a model that can do function calling with the following functions"
        + decls
        + "<end_of_turn>\n"
        "<start_of_turn>user\n" + instruction + "<end_of_turn>\n"
        "<start_of_turn>model\n"
    )


def parse_call(text):
    """Read the FIRST `call:NAME{...}` out of the model's output.

    Values wrapped in <escape> are strings; bare values are numbers or
    booleans. Returns (name, args) or (None, reason).
    """
    start = text.find(START_CALL)
    if start == -1:
        snippet = text.strip().replace("\n", " ")[:120]
        return None, "no function call in output: %r" % snippet

    rest = text[start + len(START_CALL):]
    brace = rest.find("{")
    if brace == -1:
        return None, "malformed call, no argument block"

    name = rest[:brace].strip()
    end = rest.find(END_CALL)
    body = rest[:end] if end != -1 else rest
    close = body.rfind("}")
    args_block = body[brace + 1:close] if close > brace else body[brace + 1:]

    return name, _parse_args(args_block)


def _parse_args(block):
    args = {}
    rest = block
    while rest:
        rest = rest.lstrip(", ")
        if not rest:
            break
        colon = rest.find(":")
        if colon == -1:
            break
        key = rest[:colon].strip()
        rest = rest[colon + 1:]

        if rest.startswith("<escape>"):
            rest = rest[len("<escape>"):]
            end = rest.find("<escape>")
            if end == -1:
                value, rest = rest.strip(), ""
            else:
                value, rest = rest[:end].strip(), rest[end + len("<escape>"):]
        else:
            end = rest.find(",")
            raw = (rest if end == -1 else rest[:end]).strip()
            rest = "" if end == -1 else rest[end:]
            value = _coerce(raw)

        if key:
            args[key] = value
    return args


def _coerce(raw):
    if re.fullmatch(r"-?\d+", raw):
        return int(raw)
    if re.fullmatch(r"-?\d*\.\d+", raw):
        return float(raw)
    if raw.lower() in ("true", "false"):
        return raw.lower() == "true"
    return raw


def parse_calls(text, max_calls=5):
    """Read every GENUINE call from a multi-call output.

    Measured boundary (probe, 2026-08-27): the model opens each intended
    call with `<start_function_call>` -- one per intent in a compound
    instruction -- and then keeps babbling bare `call:NAME{...}` blocks
    WITHOUT the opener. A compound "weather in Nairobi, Kisumu and Mombasa"
    yields exactly three tagged calls before the first untagged one.

    Rule: accept the contiguous head-run of tagged calls, where each tag
    follows the previous call's `<end_function_call>` with nothing in
    between. The first bare `call:` (or any other text in the gap) ends
    the run. Exact repeats are dropped as a belt against tagged runaway.
    """
    calls, seen = [], set()
    pos = text.find("<start_function_call>")
    if pos == -1:
        snippet = text.strip().replace("\n", " ")[:120]
        return [], "no function call in output: %r" % snippet

    while pos != -1 and len(calls) < max_calls:
        if not text.startswith(START_CALL, pos):
            break  # a start tag without call: -- treat as the end of sense
        rest = text[pos + len(START_CALL):]
        brace = rest.find("{")
        end = rest.find(END_CALL)
        if brace == -1 or (end != -1 and end < brace):
            break
        name = rest[:brace].strip()
        body = rest[:end] if end != -1 else rest
        close = body.rfind("}")
        args_block = body[brace + 1:close] if close > brace else body[brace + 1:]
        args = _parse_args(args_block)

        key = (name, tuple(sorted((k, str(v)) for k, v in args.items())))
        if key not in seen:
            seen.add(key)
            calls.append((name, args))

        if end == -1:
            break  # generation was cut mid-call; keep what we have
        after = pos + len(START_CALL) + end + len(END_CALL)
        # The next genuine call must start IMMEDIATELY. Anything else --
        # bare call:, prose, whitespace babble -- is the runaway tail.
        if text.startswith("<start_function_call>", after):
            pos = after
        else:
            break

    if not calls:
        return [], "no parseable call in output"
    return calls, None


def select_many(llama, instruction, tools, max_calls=5, max_tokens=192):
    """One natural-language instruction -> a batch of tool calls.

    Returns (calls, raw, error) where calls is a list of (name, args).
    Unknown tool names end the genuine run at that point; if nothing
    valid precedes them, that is the error.
    """
    prompt = build_prompt(instruction, tools)
    raw = llama.complete(prompt, temperature=0.0, max_tokens=max_tokens,
                         stop=MULTI_STOPS)
    parsed, error = parse_calls(raw, max_calls=max_calls)
    if error:
        return [], raw, error

    known = {t["name"] for t in tools}
    calls = []
    for name, args in parsed:
        if name not in known:
            break
        calls.append((name, args))
    if not calls:
        return [], raw, "picked unknown tool %r" % parsed[0][0]
    return calls, raw, None


def select(llama, instruction, tools, max_tokens=96):
    """Ask FunctionGemma to turn one normalised instruction into one call.

    Returns (name, args, raw_output, error).
    """
    prompt = build_prompt(instruction, tools)
    raw = llama.complete(prompt, temperature=0.0, max_tokens=max_tokens, stop=STOPS)

    name, args = parse_call(raw)
    if name is None:
        return None, None, raw, args

    known = {t["name"] for t in tools}
    if name not in known:
        return None, None, raw, "picked unknown tool %r" % name

    return name, args, raw, None
