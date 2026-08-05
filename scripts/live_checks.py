"""Assertion suite for a running pond-server. Driven by scripts/live-test.sh.

Drives a running pond-server over real HTTP. Covers what unit and integration
tests cannot: migrations applied to a real file on disk, a restart against a
database that already has rows, route registration, the auth middleware, and
the wiring in main.rs.

Every check asserts the STATUS CODE FIRST. An earlier version of this script
reported PASS for `body.get("profile_id") is None` against an error payload,
where every lookup returns None -- a check that passes because the request
failed reports the opposite of the truth.
"""

import json
import os
import sqlite3
import subprocess
import sys

DATA_DIR = os.environ.get("POND_DATA_DIR", "/tmp/pond-live")
DB = os.path.join(DATA_DIR, "pond_system.db")
PORT_FILE = os.path.join(DATA_DIR, ".runtime_api_port")

results = []


def check(label, ok, detail=""):
    results.append((label, bool(ok), detail))
    if len(detail) > 220:
        detail = detail[:220] + " ...(truncated)"
    print(("PASS  " if ok else "FAIL  ") + label + (("  -- " + detail) if detail else ""))
    return bool(ok)


def api_port():
    with open(PORT_FILE) as fh:
        return fh.read().strip()


def call(method, path, body=None, token=None):
    url = "http://127.0.0.1:%s%s" % (api_port(), path)
    cmd = ["curl", "-s", "-o", "/dev/stdout", "-w", "\n%{http_code}", "-X", method, url]
    if token:
        cmd += ["-H", "Authorization: Bearer " + token]
    if body is not None:
        cmd += ["-H", "Content-Type: application/json", "-d", json.dumps(body)]
    out = subprocess.run(cmd, capture_output=True, text=True).stdout
    raw, _, code = out.rpartition("\n")
    try:
        parsed = json.loads(raw) if raw.strip() else None
    except json.JSONDecodeError:
        parsed = raw
    return int(code), parsed


def expect(label, code, want, body, *predicates):
    """Status first, then body predicates. Returns True only if all held."""
    if code != want:
        check(label, False, "HTTP %s (wanted %s): %s" % (code, want, body))
        return False
    ok = True
    for sublabel, predicate in predicates:
        ok &= check(label + " / " + sublabel, predicate, str(body))
    if not predicates:
        check(label, True)
    return ok


def db():
    return sqlite3.connect(DB)


def seed_session(sid):
    con = db()
    con.execute(
        "INSERT OR REPLACE INTO sessions (id, title, created_at, updated_at) "
        "VALUES (?, 'live check', datetime('now'), datetime('now'))",
        (sid,),
    )
    con.commit()
    con.close()


def new_profile(name):
    code, body = call("POST", "/api/v1/profiles", {"display_name": name})
    if code != 201:
        check("create profile %s" % name, False, "HTTP %s: %s" % (code, body))
        return None
    return body["id"]


# ── P2: schema and provenance ────────────────────────────────────────────────


def section_schema():
    print("\n=== P2: migration 0037 on a real database file ===")
    con = db()
    rows = con.execute(
        "SELECT version, success FROM _sqlx_migrations ORDER BY version DESC LIMIT 3"
    ).fetchall()
    check("0037 applied and successful", any(r[0] == 37 and r[1] == 1 for r in rows), str(rows))
    check(
        "0037 applied exactly once",
        [r[0] for r in con.execute("SELECT version FROM _sqlx_migrations WHERE version = 37")]
        == [37],
    )
    cols = [c[1] for c in con.execute("PRAGMA table_info(sessions)").fetchall()]
    for col in ("profile_id", "identification_source", "identification_confidence"):
        check("sessions.%s present" % col, col in cols)
    trigs = [
        r[0] for r in con.execute("SELECT name FROM sqlite_master WHERE type='trigger'").fetchall()
    ]
    check(
        "delete-releases-sessions trigger installed",
        "trg_profiles_delete_releases_sessions" in trigs,
        str(trigs),
    )
    con.close()


# ── P3 / P5: identification and the strength ordering ────────────────────────


def section_identity():
    print("\n=== P3: identification, provenance, and the strength ordering ===")
    seed_session("sess-identity")

    code, body = call("GET", "/api/v1/sessions/sess-identity/user")
    expect(
        "unidentified session",
        code,
        200,
        body,
        ("reports nobody", body and body.get("profile_id") is None),
        ("reports provenance 'unknown'", body and body.get("identification_source") == "unknown"),
    )

    code, body = call("GET", "/api/v1/sessions/does-not-exist/user")
    expect(
        "GET on an unknown session is 200, not an error",
        code,
        200,
        body,
        ("says nobody", body and body.get("profile_id") is None),
    )
    code, body = call("DELETE", "/api/v1/sessions/does-not-exist/user")
    expect("DELETE on an unknown session is 404", code, 404, body)

    jerry = new_profile("Jerry")
    liz = new_profile("Liz")
    if not (jerry and liz):
        return None, None

    # P3: explicit identification (the route that did not exist before)
    code, body = call(
        "PUT", "/api/v1/sessions/sess-identity/user", {"profile_id": jerry}
    )
    expect(
        "PUT /sessions/{id}/user binds explicitly",
        code,
        200,
        body,
        ("bound", body and body.get("bound") is True),
        ("source is explicit", body and body.get("identification_source") == "explicit"),
    )

    code, body = call("GET", "/api/v1/sessions/sess-identity/user")
    expect(
        "the binding reads back",
        code,
        200,
        body,
        ("names Jerry", body and body.get("profile_id") == jerry),
        ("carries no confidence", body and body.get("confidence") is None),
    )

    # P4: the strength ordering, enforced inside the write
    con = db()
    con.execute(
        "UPDATE sessions SET identification_source = 'paired_device' WHERE id = 'sess-identity'"
    )
    con.commit()
    con.close()

    code, body = call("PUT", "/api/v1/sessions/sess-identity/user", {"profile_id": liz})
    expect(
        "a weaker source is refused",
        code,
        200,
        body,
        ("bound is false", body and body.get("bound") is False),
        ("gives a reason", body and "reason" in body),
    )
    code, body = call("GET", "/api/v1/sessions/sess-identity/user")
    expect(
        "the stronger binding survived the attempt",
        code,
        200,
        body,
        ("still Jerry, not Liz", body and body.get("profile_id") == jerry),
        ("still paired_device", body and body.get("identification_source") == "paired_device"),
    )

    code, body = call("PUT", "/api/v1/sessions/sess-identity/user", {"profile_id": "   "})
    expect("an empty profile_id is rejected", code, 400, body)
    code, body = call("PUT", "/api/v1/sessions/no-such/user", {"profile_id": jerry})
    expect("identifying a nonexistent session is 404", code, 404, body)

    return jerry, liz


# ── P7: deletion reports, and the foreign-key trap ───────────────────────────


def section_deletion(jerry, liz):
    print("\n=== P7: deleting a member reports what went, and what stayed ===")
    if not liz:
        return

    seed_session("sess-liz")
    code, body = call("PUT", "/api/v1/sessions/sess-liz/user", {"profile_id": liz})
    if not expect("bind a session to Liz", code, 200, body,
                  ("bound", body and body.get("bound") is True)):
        return

    # Make Liz the primary member, so the dangling-reference fix is exercised.
    code, _ = call("PUT", "/api/v1/settings", {"primary_profile_id": liz})
    check("set Liz as primary member", code == 200)

    code, body = call("DELETE", "/api/v1/profiles/" + liz)
    ok = expect(
        "DELETE /profiles/{id} succeeds with a live session bound",
        code,
        200,
        body,
        ("names the member deleted", body and body.get("display_name") == "Liz"),
        ("reports a released session", body and body.get("released", {}).get("sessions") == 1),
        ("reports a memory count", body and "memories" in body.get("deleted", {})),
        ("cleared the primary setting", body and body.get("cleared_primary_profile") is True),
    )
    if not ok:
        return

    code, body = call("GET", "/api/v1/sessions/sess-liz/user")
    expect(
        "the conversation survived, stripped of its attribution",
        code,
        200,
        body,
        ("no owner", body and body.get("profile_id") is None),
        ("provenance cleared too", body and body.get("identification_source") == "unknown"),
    )

    code, body = call("GET", "/api/v1/settings")
    expect(
        "primary_profile_id no longer names a deleted member",
        code,
        200,
        body,
        ("is empty", body is not None and not body.get("primary_profile_id")),
    )

    code, body = call("DELETE", "/api/v1/profiles/" + liz)
    expect("deleting the same member again is 404", code, 404, body)
    code, body = call("DELETE", "/api/v1/profiles/never-existed")
    expect("deleting a member who never existed is 404", code, 404, body)


# ── P8: legacy rows are shared context, owned by nobody ──────────────────────


def section_legacy_rows(jerry):
    print("\n=== P8: a legacy unattributed memory is shared, not owned ===")
    con = db()
    tables = [
        r[0]
        for r in con.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='memory_fragments'"
        )
    ]
    if not tables:
        check("memory_fragments table exists", False)
        con.close()
        return

    con.execute(
        "INSERT OR REPLACE INTO memory_fragments (id, profile_id, content, source, created_at) "
        "VALUES ('legacy-1', NULL, 'the spare key is under the third plant pot', 'live', datetime('now'))"
    )
    if jerry:
        con.execute(
            "INSERT OR REPLACE INTO memory_fragments (id, profile_id, content, source, created_at) "
            "VALUES ('owned-1', ?, 'my boiler code is F28', 'live', datetime('now'))",
            (jerry,),
        )
    con.commit()

    owned = con.execute(
        "SELECT COUNT(*) FROM memory_fragments WHERE profile_id = ?", (jerry,)
    ).fetchone()[0]
    shared = con.execute(
        "SELECT COUNT(*) FROM memory_fragments WHERE profile_id IS NULL"
    ).fetchone()[0]
    con.close()

    check("an owned memory is attributed", owned >= 1, "owned=%d" % owned)
    check("a legacy memory stays unattributed", shared >= 1, "shared=%d" % shared)

    # Deleting the owner must take the owned row and leave the shared one.
    if jerry:
        code, body = call("DELETE", "/api/v1/profiles/" + jerry)
        if expect("delete the owning member", code, 200, body):
            check(
                "the delete reported the owned memory",
                body.get("deleted", {}).get("memories", 0) >= 1,
                str(body.get("deleted")),
            )
            con = db()
            still_owned = con.execute(
                "SELECT COUNT(*) FROM memory_fragments WHERE id = 'owned-1'"
            ).fetchone()[0]
            still_shared = con.execute(
                "SELECT COUNT(*) FROM memory_fragments WHERE id = 'legacy-1'"
            ).fetchone()[0]
            con.close()
            check("their own memory went with them (FK cascade)", still_owned == 0)
            check(
                "the shared household memory survived",
                still_shared == 1,
                "this is the whole point of Household being a positive classification",
            )


def main():
    """Auth is NOT checked here.

    scripts/live-test.sh starts this server with POND_DEV_ALLOW_LOOPBACK so the
    functional routes are reachable. Every auth assertion would therefore pass
    regardless of what the allowlist does. The script runs a second server
    without the bypass for that section -- a check that passes because the
    bypass is on reports the opposite of the truth.
    """
    section_schema()
    jerry, liz = section_identity()
    section_deletion(jerry, liz)
    section_legacy_rows(jerry)

    failed = [label for label, ok, _ in results if not ok]
    print("\n%d checks run, %d failed" % (len(results), len(failed)))
    for f in failed:
        print("  FAILED:", f)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
