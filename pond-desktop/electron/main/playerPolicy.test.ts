import { describe, expect, it } from "vitest";
import { createRequestPolicy } from "./playerPolicy";

function server(answer: (body: { url: string; method: string }) => unknown) {
  const asked: Array<{ url: string; method: string }> = [];
  const fetchFn = (async (_url: string, init: RequestInit) => {
    const body = JSON.parse(String(init.body)) as { url: string; method: string };
    asked.push(body);
    const result = answer(body);
    if (result instanceof Error) throw result;
    return new Response(JSON.stringify(result), { status: 200 });
  }) as unknown as typeof fetch;
  return { fetchFn, asked };
}

const allow = () => ({ allowed: true });

describe("createRequestPolicy", () => {
  it("lets the app's own pages and loopback through without asking", async () => {
    const s = server(allow);
    const policy = createRequestPolicy({ serverUrl: () => "http://127.0.0.1:4000", fetchFn: s.fetchFn });

    for (const url of [
      "app://giap/player.html",
      "blob:app://giap/1234",
      "data:text/plain,x",
      "http://127.0.0.1:4000/api/v1/player/events",
      "http://localhost:1420/player.html",
      "ws://localhost:1421/",
    ]) {
      expect(await policy(url, "GET"), url).toEqual({ cancel: false });
    }
    expect(s.asked).toHaveLength(0);
  });

  it("asks the server about a host, sending the origin and never the path", async () => {
    const s = server(allow);
    const policy = createRequestPolicy({ serverUrl: () => "http://127.0.0.1:4000", fetchFn: s.fetchFn });

    const verdict = await policy("https://amp-api.music.apple.com/v1/catalog/us/search?term=secret+song", "GET");

    expect(verdict).toEqual({ cancel: false });
    expect(s.asked).toEqual([{ url: "https://amp-api.music.apple.com/", method: "GET" }]);
  });

  it("cancels what the network policy refuses", async () => {
    const s = server(() => ({ allowed: false, reason: "offline" }));
    const policy = createRequestPolicy({ serverUrl: () => "http://x", fetchFn: s.fetchFn });
    expect(await policy("https://js-cdn.music.apple.com/musickit/v3/musickit.js", "GET")).toEqual({ cancel: true });
  });

  it("asks once per host for a stretch, not once per request", async () => {
    const s = server(allow);
    let clock = 0;
    const policy = createRequestPolicy({ serverUrl: () => "http://x", fetchFn: s.fetchFn, now: () => clock, ttlMs: 60_000 });

    for (let i = 0; i < 50; i++) await policy(`https://aod-ssl.itunes.apple.com/segment-${i}.m4s`, "GET");
    expect(s.asked).toHaveLength(1);

    clock = 61_000;
    await policy("https://aod-ssl.itunes.apple.com/segment-x.m4s", "GET");
    expect(s.asked).toHaveLength(2);
  });

  it("asks separately for separate hosts", async () => {
    const s = server(allow);
    const policy = createRequestPolicy({ serverUrl: () => "http://x", fetchFn: s.fetchFn });
    await policy("https://a.apple.com/", "GET");
    await policy("https://b.apple.com/", "GET");
    expect(s.asked.map((a) => a.url)).toEqual(["https://a.apple.com/", "https://b.apple.com/"]);
  });

  it("joins requests that arrive while the first answer is on its way", async () => {
    const s = server(allow);
    const policy = createRequestPolicy({ serverUrl: () => "http://x", fetchFn: s.fetchFn });
    await Promise.all(
      Array.from({ length: 20 }, (_, i) => policy(`https://cdn.apple.com/${i}`, "GET")),
    );
    expect(s.asked).toHaveLength(1);
  });

  it("refuses a host it cannot ask about, and asks again soon", async () => {
    let up = false;
    const s = server(() => (up ? { allowed: true } : new Error("connection refused")));
    let clock = 0;
    const policy = createRequestPolicy({ serverUrl: () => "http://x", fetchFn: s.fetchFn, now: () => clock, failureTtlMs: 5_000 });

    expect(await policy("https://api.music.apple.com/", "GET")).toEqual({ cancel: true });

    up = true;
    clock = 1_000;
    expect(await policy("https://api.music.apple.com/", "GET")).toEqual({ cancel: true });

    clock = 6_000;
    expect(await policy("https://api.music.apple.com/", "GET")).toEqual({ cancel: false });
  });

  it("follows the server when its port changes", async () => {
    const urls: string[] = [];
    let base = "http://127.0.0.1:4000";
    const fetchFn = (async (url: string) => {
      urls.push(url);
      return new Response(JSON.stringify({ allowed: true }));
    }) as unknown as typeof fetch;
    const policy = createRequestPolicy({ serverUrl: () => base, fetchFn, ttlMs: 0 });

    await policy("https://a.apple.com/", "GET");
    base = "http://127.0.0.1:4001";
    await policy("https://a.apple.com/", "GET");

    expect(urls).toEqual([
      "http://127.0.0.1:4000/api/v1/player/egress-policy",
      "http://127.0.0.1:4001/api/v1/player/egress-policy",
    ]);
  });

  it("does not choke on a URL it cannot parse", async () => {
    const policy = createRequestPolicy({ serverUrl: () => "http://x", fetchFn: server(allow).fetchFn });
    expect(await policy("not a url", "GET")).toEqual({ cancel: false });
  });
});
