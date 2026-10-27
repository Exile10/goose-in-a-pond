// Two dashboard tabs in one browser profile share one stored session. Each `PondApiClient` here
// stands for one tab; both read and write the same `localStorage`, as tabs do.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PondApiClient } from "./PondApiClient";
import { ApiError } from "./types";
import { isSignInRequired } from "./hostCredential";

const BASE = "http://localhost:4000";
const SESSION = "giap-session-token";
const REFRESH = "giap-refresh-token";
const EXPIRES = "giap-token-expires-at";

function okJson(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

function refusal(status: number, error: string): Response {
  return new Response(JSON.stringify({ error }), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function bearer(init?: RequestInit): string | undefined {
  return (init?.headers as Record<string, string> | undefined)?.["Authorization"];
}

/** What another tab's write delivers to this one; a tab's own writes fire no such event. */
function storageEvent(key: string, oldValue: string | null, newValue: string | null): void {
  window.dispatchEvent(Object.assign(new Event("storage"), { key, oldValue, newValue }));
}

const future = () => String(Date.now() + 3_600_000);

let fetchMock: ReturnType<typeof vi.fn>;
const calls = (part: string) =>
  fetchMock.mock.calls.filter(([url]) => String(url).includes(part)).length;

beforeEach(() => {
  localStorage.clear();
  fetchMock = vi.fn();
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

describe("two tabs sharing one stored session", () => {
  it("a stale tab's 401 does not erase the newer session another tab stored", async () => {
    localStorage.setItem(SESSION, "T1");
    localStorage.setItem(EXPIRES, future());
    const stale = new PondApiClient(BASE);
    const fresh = new PondApiClient(BASE);
    // The other tab re-paired; this one missed the storage event and still holds T1.
    fresh.setToken("T2", new Date(Date.now() + 3_600_000).toISOString());
    fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
      if (url.includes("/handshake/")) return refusal(403, "host_credential_required");
      return bearer(init) === "Bearer T2"
        ? okJson({ status: "ok" })
        : refusal(401, "Invalid or expired token");
    });

    await expect(stale.health()).resolves.toEqual({ status: "ok" });

    expect(localStorage.getItem(SESSION)).toBe("T2");
    // It took the stored session rather than re-pairing, which would rotate T2 away.
    expect(calls("/handshake/")).toBe(0);
  });

  it("only clears the stored session while it is still the one that was rejected", async () => {
    localStorage.setItem(SESSION, "T1");
    const stale = new PondApiClient(BASE);
    const other = new PondApiClient(BASE);
    const signIn = vi.fn();
    stale.onSignInRequired(signIn);
    fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
      if (url.includes("/handshake/pairing-code")) {
        // While this tab tries to re-pair, the other tab stores a new session.
        other.setToken("T2");
        return refusal(403, "host_credential_required");
      }
      return bearer(init) === "Bearer T2"
        ? okJson({ status: "ok" })
        : refusal(401, "Invalid or expired token");
    });

    const err = await stale.health().catch((e: unknown) => e);

    expect(isSignInRequired(err)).toBe(true);
    expect(localStorage.getItem(SESSION)).toBe("T2");
    expect(signIn).toHaveBeenLastCalledWith(true);
  });

  it("a tab whose host credential was refused does not ask again on every 401", async () => {
    localStorage.setItem(SESSION, "T1");
    const stale = new PondApiClient(BASE);
    fetchMock.mockImplementation(async (url: string) =>
      url.includes("/handshake/pairing-code")
        ? refusal(403, "host_credential_required")
        : refusal(401, "Invalid or expired token"),
    );

    expect(isSignInRequired(await stale.health().catch((e: unknown) => e))).toBe(true);
    expect(isSignInRequired(await stale.health().catch((e: unknown) => e))).toBe(true);

    expect(calls("/handshake/pairing-code")).toBe(1);
  });

  it("a storage event makes the other tab adopt the session just written", async () => {
    localStorage.setItem(SESSION, "T1");
    const a = new PondApiClient(BASE);
    const b = new PondApiClient(BASE);
    b.setToken("T2");
    storageEvent(SESSION, "T1", "T2");
    fetchMock.mockResolvedValue(okJson({ status: "ok" }));

    await a.health();

    expect(bearer(fetchMock.mock.calls[0][1] as RequestInit)).toBe("Bearer T2");
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("a session another tab stores lifts this tab's sign-in notice", async () => {
    localStorage.setItem(SESSION, "T1");
    const stale = new PondApiClient(BASE);
    const signIn = vi.fn();
    stale.onSignInRequired(signIn);
    fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
      if (url.includes("/handshake/")) return refusal(403, "host_credential_required");
      return bearer(init) === "Bearer T2"
        ? okJson({ status: "ok" })
        : refusal(401, "Invalid or expired token");
    });
    await stale.health().catch(() => {});
    expect(signIn).toHaveBeenLastCalledWith(true);

    new PondApiClient(BASE).setToken("T2");
    storageEvent(SESSION, null, "T2");

    expect(signIn).toHaveBeenLastCalledWith(false);
    await expect(stale.health()).resolves.toEqual({ status: "ok" });
  });

  it("drops its copy when another tab clears the rejected session they shared", async () => {
    localStorage.setItem(SESSION, "T1");
    const a = new PondApiClient(BASE);
    localStorage.removeItem(SESSION);
    storageEvent(SESSION, "T1", null);
    fetchMock.mockResolvedValue(okJson({ status: "ok" }));

    await a.health();

    expect(bearer(fetchMock.mock.calls[0][1] as RequestInit)).toBeUndefined();
  });
});

describe("one tab on its own", () => {
  it("refreshes a rejected session and retries, as before", async () => {
    localStorage.setItem(SESSION, "T1");
    localStorage.setItem(REFRESH, "r1");
    localStorage.setItem(EXPIRES, future());
    const tab = new PondApiClient(BASE);
    const expiresAt = new Date(Date.now() + 3_600_000).toISOString();
    fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
      if (url.includes("/handshake/refresh")) {
        return okJson({ accepted: true, session_token: "T2", refresh_token: "r2", expires_at: expiresAt });
      }
      return bearer(init) === "Bearer T2"
        ? okJson({ status: "ok" })
        : refusal(401, "Invalid or expired token");
    });

    await expect(tab.health()).resolves.toEqual({ status: "ok" });

    expect(localStorage.getItem(SESSION)).toBe("T2");
    expect(localStorage.getItem(REFRESH)).toBe("r2");
  });

  it("re-pairs when it has no refresh token, and stores the new session", async () => {
    localStorage.setItem(SESSION, "T1");
    const tab = new PondApiClient(BASE);
    fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
      if (url.includes("/handshake/pairing-code")) return okJson({ code: "123456" });
      if (url.includes("/handshake/init")) {
        return okJson({ challenge_id: "c1", challenge: btoa("challenge") });
      }
      if (url.includes("/handshake/verify")) {
        return okJson({ accepted: true, session_token: "T2", refresh_token: "r2" });
      }
      return bearer(init) === "Bearer T2"
        ? okJson({ status: "ok" })
        : refusal(401, "Invalid or expired token");
    });

    await expect(tab.health()).resolves.toEqual({ status: "ok" });

    expect(localStorage.getItem(SESSION)).toBe("T2");
    expect(calls("/handshake/verify")).toBe(1);
  });

  it("still surfaces a refused host credential to the caller", async () => {
    localStorage.setItem(SESSION, "T1");
    fetchMock.mockImplementation(async (url: string) =>
      url.includes("/handshake/pairing-code")
        ? refusal(403, "host_credential_required")
        : refusal(401, "Invalid or expired token"),
    );

    const err = await new PondApiClient(BASE).health().catch((e: unknown) => e);

    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(403);
    expect(localStorage.getItem(SESSION)).toBeNull();
  });
});
