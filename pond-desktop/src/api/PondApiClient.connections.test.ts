import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PondApiClient } from "./PondApiClient";
import { ApiError } from "./types";

function okJson(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
}

/** The contract's refusals are `{"error": "<code>"}` and nothing else. */
function refusal(status: number, error: string): Response {
  return okJson({ error }, status);
}

const credential = "a".repeat(43);
let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
  fetchMock = vi.fn();
  vi.stubGlobal("fetch", fetchMock);
  sessionStorage.setItem("giap-host-credential", credential);
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
  sessionStorage.clear();
  localStorage.clear();
});

const client = () => new PondApiClient("http://localhost:4000");
const headersOf = (call: number) => fetchMock.mock.calls[call][1].headers as Record<string, string>;
const urlOf = (call: number) => String(fetchMock.mock.calls[call][0]);

/** A request that never answers, so only the client's own timeout can end it. */
function hangUntilAborted(_url: string, init: RequestInit): Promise<Response> {
  return new Promise((_resolve, reject) => {
    init.signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
  });
}

describe("getConnections()", () => {
  it("GETs /api/v1/connections with the ordinary token, not the host credential", async () => {
    const body = { hostname: "nano", wifi: {}, ethernet: [], bluetooth: {}, can_change: { allowed: true, reason: null } };
    fetchMock.mockResolvedValueOnce(okJson(body));
    expect(await client().getConnections()).toEqual(body);
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/connections");
    expect(fetchMock.mock.calls[0][1].method).toBe("GET");
    expect(headersOf(0)["X-Pond-Host-Credential"]).toBeUndefined();
  });
});

describe("scanWifi()", () => {
  it("GETs the network list and answers just the networks", async () => {
    const networks = [{ ssid: "Home", security: "wpa2", signal: 70, known: true, current: true }];
    fetchMock.mockResolvedValueOnce(okJson({ networks }));
    expect(await client().scanWifi()).toEqual(networks);
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/connections/wifi/networks");
  });

  it("answers an empty list when the body has none", async () => {
    fetchMock.mockResolvedValueOnce(new Response(null, { status: 200 }));
    expect(await client().scanWifi()).toEqual([]);
  });

  it("waits 20 seconds for a scan, not the default 30", async () => {
    vi.useFakeTimers();
    fetchMock.mockImplementation(hangUntilAborted);
    const settled = client().scanWifi().then(
      () => "answered",
      (e: unknown) => e,
    );
    await vi.advanceTimersByTimeAsync(19_900);
    const raced = await Promise.race([settled, Promise.resolve("waiting")]);
    expect(raced).toBe("waiting");
    await vi.advanceTimersByTimeAsync(200);
    const err = (await settled) as ApiError;
    expect(err).toBeInstanceOf(ApiError);
    expect(err.status).toBe(408);
  });

  it.each([
    [503, "unsupported"],
    [403, "not_permitted"],
    [409, "busy"],
  ])("keeps the contract's %i %s as the error message", async (status, code) => {
    fetchMock.mockResolvedValueOnce(refusal(status, code));
    const err = (await client().scanWifi().catch((e: unknown) => e)) as ApiError;
    expect(err).toBeInstanceOf(ApiError);
    expect(err.status).toBe(status);
    expect(err.message).toBe(code);
  });
});

describe("joinWifi()", () => {
  const request = { ssid: "Home", passphrase: "correct horse", hidden: false };
  const joined = { ssid: "Home", address: "192.168.1.11", https_port: 4443, hostname: "nano" };

  it("POSTs the body as given, with the host credential", async () => {
    fetchMock.mockResolvedValueOnce(okJson(joined));
    expect(await client().joinWifi(request)).toEqual(joined);
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/connections/wifi/join");
    expect(fetchMock.mock.calls[0][1].method).toBe("POST");
    expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual(request);
    expect(headersOf(0)["X-Pond-Host-Credential"]).toBe(credential);
  });

  it("sends no passphrase key for an open network", async () => {
    fetchMock.mockResolvedValueOnce(okJson(joined));
    await client().joinWifi({ ssid: "Cafe", hidden: false });
    expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({ ssid: "Cafe", hidden: false });
  });

  it("waits 60 seconds, which is longer than the pond's 45", async () => {
    vi.useFakeTimers();
    fetchMock.mockImplementation(hangUntilAborted);
    const settled = client().joinWifi(request).then(
      () => "answered",
      (e: unknown) => e,
    );
    await vi.advanceTimersByTimeAsync(59_900);
    expect(await Promise.race([settled, Promise.resolve("waiting")])).toBe("waiting");
    await vi.advanceTimersByTimeAsync(200);
    expect(((await settled) as ApiError).status).toBe(408);
  });

  it("reads a v1.2 pond's 422 wrong_password without re-pairing", async () => {
    fetchMock.mockImplementation(async () => refusal(422, "wrong_password"));
    const err = (await client().joinWifi(request).catch((e: unknown) => e)) as ApiError;
    expect(err).toBeInstanceOf(ApiError);
    expect(err.status).toBe(422);
    expect(err.message).toBe("wrong_password");
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(urlOf(0)).not.toContain("handshake");
  });

  it("does not re-pair or send the passphrase twice when the pond says wrong_password", async () => {
    fetchMock.mockImplementation(async () => refusal(401, "wrong_password"));
    const err = (await client().joinWifi(request).catch((e: unknown) => e)) as ApiError;
    expect(err).toBeInstanceOf(ApiError);
    expect(err.status).toBe(401);
    expect(err.message).toBe("wrong_password");
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(urlOf(0)).not.toContain("handshake");
  });

  it("re-pairs once and repeats the join for a 401 that is an expired token", async () => {
    const future = new Date(Date.now() + 3_600_000).toISOString();
    localStorage.setItem("giap-session-token", "stale");
    localStorage.setItem("giap-refresh-token", "r1");
    localStorage.setItem("giap-token-expires-at", future);
    fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
      if (url.includes("/handshake/refresh")) {
        return okJson({ accepted: true, session_token: "fresh", refresh_token: "r2", expires_at: future });
      }
      const auth = (init?.headers as Record<string, string>)["Authorization"];
      return auth === "Bearer fresh" ? okJson(joined) : refusal(401, "Invalid or expired token");
    });
    expect(await client().joinWifi(request)).toEqual(joined);
    const joins = fetchMock.mock.calls.filter(([url]) => String(url).endsWith("/wifi/join"));
    expect(joins).toHaveLength(2);
  });

  it.each([
    [400, "invalid_request"],
    [400, "unsupported_security"],
    [404, "not_found"],
    [504, "timeout"],
    [502, "no_address"],
    [403, "not_permitted"],
    [403, "not_allowed"],
    [503, "unsupported"],
    [409, "busy"],
  ])("keeps the contract's %i %s as the error message", async (status, code) => {
    fetchMock.mockResolvedValueOnce(refusal(status, code));
    const err = (await client().joinWifi(request).catch((e: unknown) => e)) as ApiError;
    expect(err.status).toBe(status);
    expect(err.message).toBe(code);
  });

  it("lets a dropped connection through as it is, for the screen to read as a pond that moved", async () => {
    fetchMock.mockRejectedValueOnce(new TypeError("Failed to fetch"));
    await expect(client().joinWifi(request)).rejects.toBeInstanceOf(TypeError);
  });
});

describe("forgetWifi()", () => {
  it("DELETEs the percent-encoded name, with the host credential and no confirm by default", async () => {
    fetchMock.mockResolvedValueOnce(new Response(null, { status: 204 }));
    await expect(client().forgetWifi("Café Wi/Fi")).resolves.toBeUndefined();
    expect(urlOf(0)).toBe(
      `http://localhost:4000/api/v1/connections/wifi/networks/${encodeURIComponent("Café Wi/Fi")}`,
    );
    expect(fetchMock.mock.calls[0][1].method).toBe("DELETE");
    expect(headersOf(0)["X-Pond-Host-Credential"]).toBe(credential);
  });

  it("adds ?confirm=true only when asked, for the network the pond is on", async () => {
    fetchMock.mockResolvedValueOnce(new Response(null, { status: 204 }));
    await client().forgetWifi("Home", true);
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/connections/wifi/networks/Home?confirm=true");
  });

  it.each([
    [404, "not_found"],
    [409, "in_use"],
    [403, "not_permitted"],
    [403, "not_allowed"],
    [503, "unsupported"],
  ])("keeps the contract's %i %s as the error message", async (status, code) => {
    fetchMock.mockResolvedValueOnce(refusal(status, code));
    const err = (await client().forgetWifi("Home").catch((e: unknown) => e)) as ApiError;
    expect(err.status).toBe(status);
    expect(err.message).toBe(code);
  });
});

describe("getSetupStatus()", () => {
  it("GETs /api/v1/setup/status", async () => {
    const card = { id: "4e81", claimed: true, claimed_at: "2026-10-18T18:06:12Z", claimed_by_this_device: false };
    fetchMock.mockResolvedValueOnce(okJson(card));
    expect(await client().getSetupStatus()).toEqual(card);
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/setup/status");
  });
});
