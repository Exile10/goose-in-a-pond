"""W3 checks for the scratch, onboarded, no-bypass server in live-test.sh.

The launcher proves process/port ownership before calling this script. Credentials
are generated through real two-phase pairing and never printed or passed to curl.
"""
import base64
import hashlib
import hmac
import http.client
import json
import os
from pathlib import Path


def run():
    data = Path(os.environ["POND_DATA_DIR"])
    port = int((data / ".runtime_api_port").read_text())
    host_credential = (data / ".runtime_host_credential").read_text().strip()

    def call(method, path, body=None, token=None, expected=200, extra=None):
        conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
        headers = {"Content-Type": "application/json", **(extra or {})}
        if token:
            headers["Authorization"] = "Bearer " + token
        try:
            conn.request(method, "/api/v1" + path, json.dumps(body) if body is not None else None, headers)
            response = conn.getresponse()
            assert response.status == expected, (method, path, response.status, expected)
            if "text/event-stream" in response.getheader("Content-Type", ""):
                return None  # Closing the connection releases the stream.
            payload = response.read()
            return json.loads(payload) if payload else None
        finally:
            conn.close()

    host_only = {"X-Pond-Host-Credential": host_credential}

    def pair(client):
        code = call("POST", "/handshake/pairing-code", extra=host_only)["code"]
        challenge = call("POST", "/handshake/init", {"client_id": client, "client_type": "gotg", "client_version": "w3-live"})
        mac = hmac.new(code.encode(), base64.b64decode(challenge["challenge"]) + client.encode(), hashlib.sha256).hexdigest()
        result = call("POST", "/handshake/verify", {"challenge_id": challenge["challenge_id"], "mac": mac, "device_name": client})
        assert result["accepted"] is True
        return result

    # Loopback alone does not mint a code: the host credential, a loopback name and a
    # first-party origin are each required.
    call("POST", "/handshake/pairing-code", expected=403)
    call("POST", "/handshake/pairing-code", expected=403, extra={"X-Pond-Host-Credential": "A" * 43})
    call("GET", "/remote-access", expected=403)
    call("POST", "/handshake/pairing-code", expected=421, extra={**host_only, "Host": "attacker.example:%d" % port})
    call("POST", "/handshake/pairing-code", expected=403, extra={**host_only, "Origin": "http://attacker.example"})
    call("GET", "/remote-access", extra=host_only)
    print("PASS  host-only routes need the host credential, a loopback Host and a first-party Origin")

    a, b = pair("w3-live-a"), pair("w3-live-b")
    for method, path in [("POST", "/transcribe"), ("GET", "/dev/goose"), ("POST", "/handshake/revoke")]:
        call(method, path, {}, expected=401)
    call("GET", "/notifications/stream?device_id=w3-live-b", token=a["session_token"], expected=403)
    call("POST", "/devices/w3-live-b/push-token", {"platform": "fcm", "token": "scratch-placeholder"}, token=a["session_token"], expected=403)
    call("DELETE", "/devices/w3-live-b/push-token", token=a["session_token"], expected=403)
    call("GET", "/notifications/stream?device_id=w3-live-a", token=a["session_token"])
    renewed = call("POST", "/handshake/refresh", {"refresh_token": a["refresh_token"]})
    assert renewed["accepted"] is True
    call("POST", "/handshake/revoke", {"token": b["session_token"]}, token=renewed["session_token"])
    call("GET", "/settings", token=renewed["session_token"], expected=401)
    call("GET", "/settings", token=b["session_token"])
    refused = call("POST", "/handshake/refresh", {"refresh_token": renewed["refresh_token"]})
    assert refused["accepted"] is False
    call("POST", "/handshake/revoke", token=b["session_token"])
    print("PASS  W3 live pairing, delivery ownership, refresh and bearer-only revocation")


if __name__ == "__main__":
    run()
