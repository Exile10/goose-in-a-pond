#!/usr/bin/env python3
"""Web console for the dual-wield loop.

    python3 web.py            then open http://localhost:8093

Streams the hand-off live over server-sent events, so you watch E2B think,
hand an instruction to FunctionGemma, and see what FunctionGemma does with
it -- including when it overrules the tool the planner named.

Stdlib only. The page talks to this process; this process talks to the two
llama-servers.
"""

import argparse
import json
import os
import queue
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

from dualgemma import agent, tools
from dualgemma.llama import Llama


class StreamingPlanner(Llama):
    """Planner client that streams deltas to a callback while still returning
    the full reply to the agent loop. The loop stays oblivious; the browser
    sees tokens the moment they decode, which is the honest TTFT."""

    on_delta = None

    def chat(self, messages, temperature=0.0, max_tokens=1024, stop=None):
        return self.chat_stream(messages, temperature=temperature,
                                max_tokens=max_tokens, on_delta=self.on_delta)

HERE = os.path.dirname(os.path.abspath(__file__))
PLANNER = None
PLANNER_THINK = None
PICKER = None
BACKEND_LABEL = ""


def sse(event, payload):
    return "event: %s\ndata: %s\n\n" % (event, json.dumps(payload))


def run_stream(question, arm, out):
    """Run one arm, pushing every hand-off onto `out` as it happens."""
    client = PLANNER_THINK if arm == "natural" else PLANNER
    started = time.time()
    tools.reset()

    def trace(who, kind, text, extra=None):
        item = {"who": who, "kind": kind, "text": str(text), "t": round(time.time() - started, 2)}
        if extra:
            item.update(extra)
        out.put(sse("step", item))

    last_named = {"tool": None}

    def wrapped(who, kind, text):
        """Enrich the raw trace so the page can show who overruled whom."""
        extra = {}
        if who == "planner" and getattr(client, "last", None):
            extra["metrics"] = client.last
        if kind == "intent":
            last_named["tool"] = agent.named_tool(text)
            extra["named"] = last_named["tool"]
        elif kind == "call":
            chosen = str(text).split("(", 1)[0]
            extra["tool"] = chosen
            if arm == "dual" and last_named["tool"]:
                extra["named"] = last_named["tool"]
                extra["agreed"] = chosen == last_named["tool"]
        elif kind == "observation":
            extra["error"] = str(text).startswith("TOOL_ERROR")
        trace(who, kind, text, extra)

    out.put(sse("start", {"arm": arm, "question": question}))

    def forward(kind, piece):
        out.put(sse("token", {"kind": kind, "text": piece}))

    client.on_delta = forward
    try:
        if arm == "dual":
            result = agent.run_dual(PLANNER, PICKER, question, trace=wrapped)
        elif arm == "natural":
            result = agent.run_natural(PLANNER_THINK, PICKER, question, trace=wrapped)
        else:
            result = agent.run_solo(PLANNER, question, trace=wrapped)
    except Exception as exc:
        out.put(sse("failed", {"error": str(exc)}))
        return
    finally:
        client.on_delta = None

    honoured, measurable = (result.agreement if arm in ("dual", "natural") else (0, 0))
    out.put(sse("done", {
        "arm": arm,
        "answer": result.answer,
        "seconds": round(time.time() - started, 2),
        "calls": result.calls,
        "errors": result.errors,
        "handoffs": result.handoffs,
        "honoured": honoured,
        "measurable": measurable,
    }))


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def _send(self, code, body, ctype="text/html; charset=utf-8"):
        raw = body.encode() if isinstance(body, str) else body
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self):
        parsed = urlparse(self.path)
        if parsed.path == "/":
            with open(os.path.join(HERE, "web", "index.html")) as handle:
                self._send(200, handle.read())
            return

        if parsed.path == "/tools":
            self._send(200, json.dumps([
                {"name": t["name"], "description": t["description"],
                 "params": list(t["parameters"]), "required": t["required"]}
                for t in tools.TOOLS]), "application/json")
            return

        if parsed.path == "/health":
            self._send(200, json.dumps({
                "planner": PLANNER.health(), "picker": PICKER.health(),
                "think": PLANNER.think, "backend": BACKEND_LABEL,
            }), "application/json")
            return

        if parsed.path == "/stream":
            params = parse_qs(parsed.query)
            question = (params.get("q") or [""])[0].strip()
            arms = (params.get("arm") or ["dual"])[0]
            if not question:
                self._send(400, "missing q", "text/plain")
                return

            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.send_header("Connection", "keep-alive")
            self.end_headers()

            out = queue.Queue()

            def work():
                if arms == "both":
                    sequence = ["dual", "solo"]
                elif arms == "all":
                    sequence = ["dual", "solo", "natural"]
                else:
                    sequence = [arms]
                for arm in sequence:
                    run_stream(question, arm, out)
                out.put(None)

            threading.Thread(target=work, daemon=True).start()
            while True:
                try:
                    chunk = out.get(timeout=120)
                except queue.Empty:
                    break
                if chunk is None:
                    break
                try:
                    self.wfile.write(chunk.encode())
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    return
            try:
                self.wfile.write(b"event: end\ndata: {}\n\n")
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass
            return

        self._send(404, "not found", "text/plain")


def main():
    global PLANNER, PLANNER_THINK, PICKER, BACKEND_LABEL
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=8093)
    parser.add_argument("--backend-label", default="",
                        help="shown in the header, e.g. 'Jetson Orin Nano'")
    parser.add_argument("--no-think", action="store_true",
                        help="disable the planner's thinking channel (fixed per session)")
    parser.add_argument("--planner-port", type=int, default=8091)
    parser.add_argument("--picker-port", type=int, default=8092)
    opts = parser.parse_args()

    BACKEND_LABEL = opts.backend_label
    PLANNER = StreamingPlanner("http://localhost:%d" % opts.planner_port, "gemma-4-E2B-it", think=not opts.no_think)
    PLANNER_THINK = StreamingPlanner("http://localhost:%d" % opts.planner_port, "gemma-4-E2B-it", think=True)
    PICKER = Llama("http://localhost:%d" % opts.picker_port, "functiongemma-270m")

    for name, client in (("planner", PLANNER), ("picker", PICKER)):
        print("  %-8s %s  %s" % (name, "UP  " if client.health() else "DOWN", client.base_url))
    print("\n  console on http://localhost:%d\n" % opts.port)

    ThreadingHTTPServer(("127.0.0.1", opts.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
