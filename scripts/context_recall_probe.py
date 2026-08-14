#!/usr/bin/env python3
"""Drive a real pond and ask whether the ASSISTANT can reach the personal-context index.

Unit tests prove the index returns the right rows. They say nothing about whether
the thing a member actually talks to ever asks it. This plants one fact in each
corpus, then puts a question to the live agent whose answer is only obtainable
from that fact, and reports what came back.

# The experimental design, and why it is not just "does it answer"

Memories already reach the prompt through the OLD path -- `GooseAdapter::
topical_memories` reads `memory_fragments` directly and injects the top hits. So a
correct answer about a memory proves nothing about the index.

Context items and summaries are NOT on that path. Nothing injects them. The only
way the assistant can answer a question about one is by calling the
`giap-context__recall` tool, which is the phase F/G surface over the unified
index. So:

  * memory   -> CONTROL. Should work with or without the index.
  * context  -> only reachable via recall.
  * summary  -> only reachable via recall.

A pass on context/summary is evidence the whole chain works end to end. A failure
tells you WHERE it broke, because the script separately checks that the row was
indexed and that the tool was offered.

Usage:
    POND_DATA_DIR=/tmp/probe python3 scripts/context_recall_probe.py [--port 4010]

Requires a built `target/debug/pond-server`, a GGUF embedding model already in
`$POND_DATA_DIR/models/embedding/`, and a local chat model in
`$POND_DATA_DIR/models/gguf/`.
"""

import argparse
import json
import os
import shutil
import signal
import sqlite3
import subprocess
import sys
import time
import urllib.error
import urllib.request

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# The planted facts. Each is phrased so the QUESTION shares no content words with
# the ANSWER -- a keyword matcher cannot bridge them, so a correct answer is
# evidence of semantic retrieval rather than of string matching.
PLANTS = [
    {
        "corpus": "memory",
        "reachable_without_index": True,
        "text": "The spare key is under the third plant pot by the back door.",
        "question": "I am locked out. How do I get in?",
        "expect_any": ["plant pot", "third plant", "spare key", "back door"],
    },
    {
        "corpus": "context",
        "reachable_without_index": False,
        "text": "Back door opened at 19:42 while nobody was home.",
        "question": "Did anything happen at the house while it was empty?",
        "expect_any": ["19:42", "back door", "opened"],
    },
    {
        "corpus": "summary",
        "reachable_without_index": False,
        "text": (
            "We agreed to replace the shed roof before winter and to plant "
            "tomatoes in the spring."
        ),
        "question": "What did we decide about the garden?",
        "expect_any": ["shed roof", "tomato", "winter", "spring"],
    },
]


def api(port, method, path, body=None, timeout=300):
    url = f"http://127.0.0.1:{port}/api/v1{path}"
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    if data:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()
    except Exception as e:  # connection refused while starting
        return 0, str(e)


def wait_healthy(port, secs=240):
    for _ in range(secs // 3):
        if api(port, "GET", "/health", timeout=3)[0] == 200:
            return True
        time.sleep(3)
    return False


class Pond:
    """The server, owned so it is always torn down."""

    def __init__(self, data_dir, port):
        self.data_dir, self.port, self.proc = data_dir, port, None

    def start(self, log_name):
        env = dict(os.environ, POND_DATA_DIR=self.data_dir, POND_DEV_ALLOW_LOOPBACK="1")
        log = open(os.path.join(self.data_dir, log_name), "w")
        # stdin kept open: `serve` exits on EOF when detached.
        self.proc = subprocess.Popen(
            [os.path.join(REPO, "target/debug/pond-server"), "serve", "--port", str(self.port)],
            stdin=subprocess.PIPE, stdout=log, stderr=log, env=env, cwd=REPO,
        )
        return wait_healthy(self.port)

    def stop(self):
        if self.proc and self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        self.proc = None


def sql(db, statement, args=()):
    con = sqlite3.connect(db)
    try:
        cur = con.execute(statement, args)
        rows = cur.fetchall()
        con.commit()
        return rows
    finally:
        con.close()


def plant(data_dir):
    """Put one fact in each corpus, by the most direct route available.

    Written straight to the stores rather than through the API on purpose: this
    probe is about RETRIEVAL, and going through ingest would also be testing the
    producer's allow-lists. The maintenance sweep indexes whatever is there.
    """
    sysdb = os.path.join(data_dir, "pond_system.db")
    sql(sysdb, "INSERT OR IGNORE INTO profiles (id, display_name) VALUES ('jerry','Jerry')")

    for p in PLANTS:
        if p["corpus"] == "memory":
            sql(sysdb,
                "INSERT OR REPLACE INTO memory_fragments"
                " (id, profile_id, content, source, tags, created_at, access_count, lifecycle)"
                " VALUES ('probe-mem','jerry',?, 'chat','[]',datetime('now'),0,'active')",
                (p["text"],))
        elif p["corpus"] == "context":
            sql(sysdb,
                "INSERT OR IGNORE INTO context_sources"
                " (id, kind, provider, profile_id, scopes, status, created_at)"
                " VALUES ('probe-src','sensor','pond','jerry','[]','connected',"
                "  strftime('%Y-%m-%dT%H:%M:%SZ','now'))")
            sql(sysdb,
                "INSERT OR REPLACE INTO context_items"
                " (id, source_id, external_id, profile_id, source_kind, item_kind,"
                "  occurred_at, ingested_at, title, body, participants, sensitivity)"
                " VALUES ('probe-ctx','probe-src','evt-1','jerry','sensor','event',"
                "  strftime('%Y-%m-%dT%H:%M:%SZ','now'), strftime('%Y-%m-%dT%H:%M:%SZ','now'),"
                "  'back door', ?, '[]', 'internal')",
                (p["text"],))
        else:
            sql(sysdb,
                "INSERT OR REPLACE INTO sessions (id, created_at, profile_id)"
                " VALUES ('probe-sess', datetime('now'), 'jerry')")
            # Attributed deliberately: an unattributed session is a guest session
            # and retrieval refuses its summary by design.
            sql(sysdb,
                "UPDATE sessions SET rolling_summary = ?,"
                " rolling_summary_updated_at = datetime('now') WHERE id='probe-sess'",
                (p["text"],))


def index_state(data_dir):
    vdb = os.path.join(data_dir, "pond_vectors.db")
    if not os.path.exists(vdb):
        return {}
    return dict(sql(vdb, "SELECT corpus, COUNT(*) FROM vectors GROUP BY corpus"))


def ask(port, question, timeout=600):
    """One chat turn. Returns (answer_text, raw_events)."""
    url = f"http://127.0.0.1:{port}/api/v1/chat/stream"
    body = json.dumps({"message": question}).encode()
    req = urllib.request.Request(url, data=body, method="POST")
    req.add_header("Content-Type", "application/json")
    text, events = [], []
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            for raw in r:
                line = raw.decode(errors="replace").strip()
                if not line.startswith("data: "):
                    continue
                events.append(line[6:])
                try:
                    ev = json.loads(line[6:])
                except json.JSONDecodeError:
                    continue
                if ev.get("type") == "text" and ev.get("content"):
                    text.append(ev["content"])
    except Exception as e:
        events.append(f"<transport error: {e}>")
    return "".join(text), events


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=4010)
    ap.add_argument("--keep", action="store_true", help="leave the server running")
    args = ap.parse_args()

    data_dir = os.environ.get("POND_DATA_DIR")
    if not data_dir:
        sys.exit("set POND_DATA_DIR")
    os.makedirs(data_dir, exist_ok=True)

    pond = Pond(data_dir, args.port)
    try:
        print("== 1. boot, configure, onboard ==")
        if not pond.start("probe-boot.log"):
            sys.exit("server did not become healthy")
        api(args.port, "PUT", "/settings", {
            "embedding_provider": "gguf",
            "chat_provider": "local",
            "chat_model": os.environ.get("PROBE_CHAT_MODEL", "gemma-4-E2B-it-Q4_K_M"),
            # The recall tool lives behind this and ships OFF.
            "ext_context_enabled": True,
        })
        api(args.port, "POST", "/onboard/complete", {})

        print("== 2. plant one fact per corpus ==")
        plant(data_dir)
        pond.stop()

        print("== 3. restart; wait for the index to pick them up ==")
        if not pond.start("probe-run.log"):
            sys.exit("server did not come back")
        deadline = time.time() + 240
        while time.time() < deadline:
            st = index_state(data_dir)
            if st.get("memory") and st.get("context") and st.get("summary"):
                break
            time.sleep(5)
        state = index_state(data_dir)
        print(f"   indexed: {state}")

        print("== 4. ask the agent ==")
        results = []
        for p in PLANTS:
            answer, events = ask(args.port, p["question"])
            hay = answer.lower()
            hit = any(k.lower() in hay for k in p["expect_any"])
            called = any("recall" in e for e in events)
            indexed = bool(state.get(p["corpus"]))
            results.append((p, hit, called, indexed, answer))
            print(f"\n   [{p['corpus']}] {p['question']}")
            print(f"     indexed={indexed} recall_tool_seen={called} found_fact={hit}")
            print(f"     answer: {answer.strip()[:240] or '(empty)'}")

        print("\n== verdict ==")
        for p, hit, called, indexed, _ in results:
            need = "control (old prompt path can supply this)" if p["reachable_without_index"] \
                   else "ONLY reachable through the index"
            print(f"   {p['corpus']:8} indexed={str(indexed):5} answered={str(hit):5}  {need}")
        proved = [r for r in results if r[1] and not r[0]["reachable_without_index"]]
        if proved:
            print(f"\n   {len(proved)} corpus/corpora answered that ONLY the index could supply.")
        else:
            print("\n   No index-only corpus was answered. Check indexed= above:")
            print("   false -> indexing gap; true -> the agent never asked the index.")
    finally:
        if not args.keep:
            pond.stop()
        else:
            print(f"\n(server left running on :{args.port})")


if __name__ == "__main__":
    main()
