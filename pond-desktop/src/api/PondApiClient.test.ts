import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PondApiClient } from "./PondApiClient";
import { ApiError } from "./types";

// ── fetch mock helpers ────────────────────────────────────────────────────────

function okJson(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function errJson(status: number, message: string): Response {
  return new Response(JSON.stringify({ message }), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
  fetchMock = vi.fn();
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

// ── helpers ───────────────────────────────────────────────────────────────────

function client() {
  return new PondApiClient("http://localhost:4000");
}

// ── health ────────────────────────────────────────────────────────────────────

describe("health()", () => {
  it("returns HealthResponse on 200", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ status: "ok", version: "1.0" }));
    const res = await client().health();
    expect(res.status).toBe("ok");
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:4000/api/v1/health",
      expect.objectContaining({ method: "GET" }),
    );
  });

  it("throws ApiError on non-2xx", async () => {
    fetchMock.mockResolvedValueOnce(errJson(503, "unavailable"));
    await expect(client().health()).rejects.toBeInstanceOf(ApiError);
  });
});

// ── settings ──────────────────────────────────────────────────────────────────

describe("getSettings()", () => {
  it("GETs /api/v1/settings", async () => {
    const payload = { assistant_name: "Goose", user_name: "Jerry", agent_memory_inject: true, prompt_style: "balanced" };
    fetchMock.mockResolvedValueOnce(okJson(payload));
    const s = await client().getSettings();
    expect(s.assistant_name).toBe("Goose");
  });
});

describe("updateSettings()", () => {
  it("PUTs the partial patch and returns updated settings", async () => {
    const payload = { assistant_name: "Puck", user_name: "Jerry", agent_memory_inject: false, prompt_style: "concise" };
    fetchMock.mockResolvedValueOnce(okJson(payload));
    const s = await client().updateSettings({ assistant_name: "Puck" });
    expect(s.assistant_name).toBe("Puck");
    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(init.method).toBe("PUT");
    expect(JSON.parse(init.body as string)).toMatchObject({ assistant_name: "Puck" });
  });
});

// ── devices ───────────────────────────────────────────────────────────────────

describe("listDevices()", () => {
  it("returns an array of devices", async () => {
    fetchMock.mockResolvedValueOnce(okJson([{ id: "d1", name: "Lamp", is_online: true }]));
    const devices = await client().listDevices();
    expect(devices).toHaveLength(1);
    expect(devices[0].name).toBe("Lamp");
  });
});

// ── memory ────────────────────────────────────────────────────────────────────

describe("listMemories()", () => {
  it("appends limit query param", async () => {
    fetchMock.mockResolvedValueOnce(okJson([]));
    await client().listMemories(10);
    expect(fetchMock.mock.calls[0][0]).toContain("limit=10");
  });
});

describe("addMemory()", () => {
  it("POSTs content and optional tags", async () => {
    const frag = { id: "m1", content: "prefer Celsius", tags: ["prefs"], created_at: "2026-01-01" };
    fetchMock.mockResolvedValueOnce(okJson(frag));
    const res = await client().addMemory("prefer Celsius", ["prefs"]);
    expect(res.content).toBe("prefer Celsius");
    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(init.body as string)).toEqual({ content: "prefer Celsius", tags: ["prefs"] });
  });
});

describe("deleteMemory()", () => {
  it("DELETEs /api/v1/memories/:id", async () => {
    fetchMock.mockResolvedValueOnce(new Response(null, { status: 204 }));
    // 204 has no body — mock request as ok
    fetchMock.mockResolvedValueOnce(okJson({}));
    fetchMock.mockReset();
    fetchMock.mockResolvedValueOnce(okJson({}));
    await client().deleteMemory("m1");
    expect(fetchMock.mock.calls[0][0]).toContain("/memories/m1");
    expect((fetchMock.mock.calls[0][1] as RequestInit).method).toBe("DELETE");
  });
});

// ── skills ────────────────────────────────────────────────────────────────────

describe("listSkills()", () => {
  it("adds ?all=true when requested", async () => {
    fetchMock.mockResolvedValueOnce(okJson([]));
    await client().listSkills(true);
    expect(fetchMock.mock.calls[0][0]).toContain("all=true");
  });

  it("omits query param when all=false", async () => {
    fetchMock.mockResolvedValueOnce(okJson([]));
    await client().listSkills(false);
    expect(fetchMock.mock.calls[0][0]).not.toContain("all=true");
  });
});

// ── prompts ───────────────────────────────────────────────────────────────────

describe("updatePrompt()", () => {
  it("PUTs content to the named prompt endpoint", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ name: "balanced", content: "new content", is_system: true }));
    const res = await client().updatePrompt("balanced", "new content");
    expect(res.content).toBe("new content");
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toContain("/prompts/balanced");
    expect(init.method).toBe("PUT");
  });
});

// ── agent extras ──────────────────────────────────────────────────────────────

describe("listExtras()", () => {
  it("GETs /api/v1/agent/extras", async () => {
    fetchMock.mockResolvedValueOnce(okJson([{ key: "tz", content: "Africa/Nairobi", enabled: true }]));
    const extras = await client().listExtras();
    expect(extras[0].key).toBe("tz");
  });
});

describe("addExtra()", () => {
  it("POSTs key + content", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ key: "tz", content: "Africa/Nairobi", enabled: true }));
    await client().addExtra("tz", "Africa/Nairobi");
    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(init.body as string)).toEqual({ key: "tz", content: "Africa/Nairobi" });
  });
});

describe("deleteExtra()", () => {
  it("URL-encodes the key", async () => {
    fetchMock.mockResolvedValueOnce(okJson({}));
    await client().deleteExtra("skill:morning");
    expect(fetchMock.mock.calls[0][0]).toContain("skill%3Amorning");
  });
});

// ── authorization header ──────────────────────────────────────────────────────

describe("setToken()", () => {
  it("attaches Bearer token to subsequent requests", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ status: "ok" }));
    const c = client();
    c.setToken("my-token");
    await c.health();
    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect((init.headers as Record<string, string>)["Authorization"]).toBe("Bearer my-token");
  });

  it("omits Authorization header when token is null", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ status: "ok" }));
    const c = client();
    c.setToken(null);
    await c.health();
    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect((init.headers as Record<string, string>)["Authorization"]).toBeUndefined();
  });
});

// ── ApiError ──────────────────────────────────────────────────────────────────

describe("ApiError", () => {
  it("captures status and message", async () => {
    fetchMock.mockResolvedValueOnce(errJson(404, "not found"));
    try {
      await client().getPrompt("missing");
      expect.fail("should have thrown");
    } catch (e) {
      expect(e).toBeInstanceOf(ApiError);
      expect((e as ApiError).status).toBe(404);
      expect((e as ApiError).message).toBe("not found");
    }
  });

  it("falls back to statusText when response body is not JSON", async () => {
    fetchMock.mockResolvedValueOnce(new Response("Bad Gateway", { status: 502, statusText: "Bad Gateway" }));
    await expect(client().health()).rejects.toMatchObject({ status: 502 });
  });
});

// ── chatStream ────────────────────────────────────────────────────────────────

describe("chatStream()", () => {
  function makeStream(lines: string[]): ReadableStream<Uint8Array> {
    const encoder = new TextEncoder();
    return new ReadableStream({
      start(controller) {
        for (const line of lines) {
          controller.enqueue(encoder.encode(line + "\n"));
        }
        controller.close();
      },
    });
  }

  it("yields text events from SSE stream", async () => {
    const sseLines = [
      'data: {"type":"text","content":"Hello"}',
      'data: {"type":"text","content":" world"}',
      "data: [DONE]",
    ];
    fetchMock.mockResolvedValueOnce(
      new Response(makeStream(sseLines), { status: 200 }),
    );

    const events = [];
    for await (const ev of client().chatStream("hi")) {
      events.push(ev);
    }

    expect(events.filter((e) => e.type === "text")).toHaveLength(2);
    expect(events.find((e) => e.type === "done")).toBeDefined();
  });

  it("throws ApiError when chat endpoint returns non-2xx", async () => {
    fetchMock.mockResolvedValueOnce(errJson(401, "unauthorized"));
    const gen = client().chatStream("hi");
    await expect(gen.next()).rejects.toBeInstanceOf(ApiError);
  });

  it("skips malformed SSE lines without throwing", async () => {
    const sseLines = [
      "data: not-valid-json",
      'data: {"type":"text","content":"ok"}',
    ];
    fetchMock.mockResolvedValueOnce(
      new Response(makeStream(sseLines), { status: 200 }),
    );

    const events = [];
    for await (const ev of client().chatStream("hi")) {
      events.push(ev);
    }
    expect(events.filter((e) => e.type === "text")).toHaveLength(1);
  });
});
