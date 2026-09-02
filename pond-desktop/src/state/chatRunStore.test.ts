/**
 * The turn outlives the view.
 *
 * `GuiMode` swaps sections with a `switch`, so every sidebar press unmounts
 * `<Chat />`. These tests drive the store directly, with no component mounted,
 * because that is exactly the condition it exists for: a turn still streaming
 * while nothing is there to show it.
 *
 * The one that matters most — "keeps folding frames after the last subscriber
 * leaves" — does mount, through `useChatRun`, and then unmounts, because a real
 * subscriber leaving is the event under test and a hand-rolled stand-in for it
 * would be testing the stand-in. Before this store, the frames that arrived
 * after that point were decoded and thrown away: the answer arrived, was
 * written to the database, and was invisible to the person who asked for it.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook } from "@testing-library/react";
import { api } from "../api/PondApiClient";
import {
  __resetChatRunForTests,
  setChatRunBridge,
  useChatRun,
  getChatRun,
  sendTurn,
  hasLiveThread,
  acknowledgeCompletion,
  resetConversation,
  truncateFrom,
  patchMessage,
} from "./chatRunStore";
import type { ChatRunBridge } from "./chatRunStore";
import type { ChatEvent } from "../api/types";

vi.mock("../api/PondApiClient", () => ({
  api: {
    chatStream: vi.fn(),
    setToken: vi.fn(),
    getSessionMessages: vi.fn(),
    sessionAttachmentUrl: vi.fn(
      (sessionId: string, id: string) => `/api/v1/sessions/${sessionId}/attachments/${id}`,
    ),
  },
}));

// ── Helpers ───────────────────────────────────────────────────────────────────

/** Yield the given frames, then finish. */
function stream(events: ChatEvent[]): AsyncGenerator<ChatEvent> {
  return (async function* () {
    for (const ev of events) yield ev;
  })();
}

/**
 * A stream held open until the test says otherwise.
 *
 * `push` delivers one frame, `end` closes it. Both resolve once the driver has
 * actually consumed the frame, so a test can unsubscribe at a known point in
 * the middle of a turn rather than racing it.
 */
function deferredStream() {
  const pending: ChatEvent[] = [];
  let wake: (() => void) | null = null;
  let done = false;

  const gen = (async function* () {
    for (;;) {
      while (pending.length > 0) yield pending.shift()!;
      if (done) return;
      await new Promise<void>((r) => { wake = r; });
    }
  })();

  return {
    gen,
    async push(ev: ChatEvent) {
      pending.push(ev);
      wake?.(); wake = null;
      await flush();
    },
    async end() {
      done = true;
      wake?.(); wake = null;
      await flush();
    },
  };
}

/** Let every already-scheduled microtask and macrotask settle. */
async function flush(): Promise<void> {
  for (let i = 0; i < 5; i += 1) await new Promise((r) => setTimeout(r, 0));
}

function bridge(over: Partial<ChatRunBridge> = {}): ChatRunBridge {
  return {
    sessionToken: "test-token",
    serverOnline: true,
    onSessionId: vi.fn(),
    onResponseMeta: vi.fn(),
    onContextCard: vi.fn(),
    ...over,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  __resetChatRunForTests();
  setChatRunBridge(bridge());
  vi.mocked(api.getSessionMessages).mockResolvedValue([]);
});

// ── Starting a turn ───────────────────────────────────────────────────────────

describe("starting a turn", () => {
  it("claims the turn before it awaits anything", () => {
    vi.mocked(api.chatStream).mockReturnValue(stream([]) as never);
    sendTurn({ text: "hello" });

    // Synchronously, on the same tick: two sends in one tick must not both run.
    const run = getChatRun();
    expect(run.busy).toBe(true);
    expect(run.messages.map((m) => m.role)).toEqual(["user", "agent"]);
    expect(run.messages[0].text).toBe("hello");
    expect(run.messages[1].streaming).toBe(true);
  });

  it("holds a second message rather than starting a second run", () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValue(held.gen as never);

    sendTurn({ text: "first" });
    sendTurn({ text: "second" });

    expect(getChatRun().queued).toEqual(["second"]);
    expect(api.chatStream).toHaveBeenCalledTimes(1);
  });

  it("refuses a turn with neither words nor images", () => {
    sendTurn({ text: "   " });
    expect(api.chatStream).not.toHaveBeenCalled();
    expect(getChatRun().busy).toBe(false);
  });
});

// ── The reason this store exists ──────────────────────────────────────────────

describe("a turn nobody is watching", () => {
  it("keeps folding frames after the last subscriber leaves", async () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValue(held.gen as never);

    // A mounted surface, subscribed exactly the way a real one is.
    const mounted = renderHook(() => useChatRun());

    sendTurn({ text: "why do geese fly in a V" });
    await held.push({ type: "text", content: "Because " } as ChatEvent);
    expect(mounted.result.current.messages[1].text).toBe("Because ");

    // The sidebar press.
    mounted.unmount();

    await held.push({ type: "text", content: "it saves " } as ChatEvent);
    await held.push({ type: "text", content: "energy." } as ChatEvent);
    await held.end();

    const run = getChatRun();
    expect(run.messages[1].text).toBe("Because it saves energy.");
    expect(run.messages[1].streaming).toBe(false);
    expect(run.busy).toBe(false);
  });

  it("drains its queue with nothing mounted", async () => {
    const first = deferredStream();
    vi.mocked(api.chatStream).mockReturnValueOnce(first.gen as never);
    vi.mocked(api.chatStream).mockReturnValueOnce(stream([
      { type: "text", content: "and second" } as ChatEvent,
    ]) as never);

    sendTurn({ text: "first" });
    sendTurn({ text: "second" });
    await first.push({ type: "text", content: "first answer" } as ChatEvent);
    await first.end();

    expect(api.chatStream).toHaveBeenCalledTimes(2);
    expect(vi.mocked(api.chatStream).mock.calls[1][0]).toBe("second");
    expect(getChatRun().queued).toEqual([]);
    expect(getChatRun().busy).toBe(false);
  });
});

// ── The bridge ────────────────────────────────────────────────────────────────

describe("talking back to the app", () => {
  it("forwards the session and the model that answered", async () => {
    const b = bridge();
    setChatRunBridge(b);
    vi.mocked(api.chatStream).mockReturnValue(stream([
      {
        done: true,
        session_id: "sess-9",
        model_role: "chat",
        model_name: "gemma",
        usage: { prompt_tokens: 5, completion_tokens: 7 },
      } as ChatEvent,
    ]) as never);

    sendTurn({ text: "hi" });
    await flush();

    expect(b.onSessionId).toHaveBeenCalledWith("sess-9");
    expect(b.onResponseMeta).toHaveBeenCalledWith({
      modelName: "gemma",
      modelRole: "chat",
      completionTokens: 7,
    });
    expect(getChatRun().sessionId).toBe("sess-9");
  });

  it("forwards a tool call as a context card", async () => {
    const b = bridge();
    setChatRunBridge(b);
    vi.mocked(api.chatStream).mockReturnValue(stream([
      { type: "tool_call", tool: "get_current_weather", id: "t1" } as ChatEvent,
    ]) as never);

    sendTurn({ text: "weather?" });
    await flush();

    expect(b.onContextCard).toHaveBeenCalledTimes(1);
    expect(getChatRun().messages[1].cards?.[0].tool).toBe("get_current_weather");
    expect(getChatRun().messages[1].status).toBe("Checking the weather…");
  });

  it("still sends when no provider is mounted to bridge it", async () => {
    // The client holds its own token and refreshes it, so a bridgeless send
    // degrades to "the client authenticates itself", not to a failed send.
    __resetChatRunForTests();
    vi.mocked(api.chatStream).mockReturnValue(stream([
      { type: "text", content: "fine" } as ChatEvent,
      { done: true, session_id: "s" } as ChatEvent,
    ]) as never);

    expect(() => sendTurn({ text: "hi" })).not.toThrow();
    await flush();
    expect(vi.mocked(api.chatStream).mock.calls[0][2]).toBeUndefined();
    expect(getChatRun().messages[1].text).toBe("fine");
  });
});

// ── An offline server ─────────────────────────────────────────────────────────

describe("a queue held through an outage", () => {
  it("waits for the server rather than dropping what was typed", async () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValueOnce(held.gen as never);

    sendTurn({ text: "first" });
    sendTurn({ text: "second" });

    setChatRunBridge(bridge({ serverOnline: false }));
    await held.end();

    expect(api.chatStream).toHaveBeenCalledTimes(1);
    expect(getChatRun().queued).toEqual(["second"]);

    vi.mocked(api.chatStream).mockReturnValueOnce(stream([]) as never);
    setChatRunBridge(bridge({ serverOnline: true }));
    await flush();

    expect(api.chatStream).toHaveBeenCalledTimes(2);
    expect(getChatRun().queued).toEqual([]);
  });
});

// ── Where a surface lands when it opens ───────────────────────────────────────

describe("hasLiveThread", () => {
  it("is true while writing, stays true until somebody has read it", async () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValue(held.gen as never);

    expect(hasLiveThread()).toBe(false);

    sendTurn({ text: "hi" });
    expect(hasLiveThread()).toBe(true);

    await held.end();
    // Finished, and nothing was mounted to show it -- still the thing you came
    // back for.
    expect(hasLiveThread()).toBe(true);

    acknowledgeCompletion();
    expect(hasLiveThread()).toBe(false);
  });

  it("comes back for the next turn", async () => {
    vi.mocked(api.chatStream).mockReturnValue(stream([]) as never);
    sendTurn({ text: "one" });
    await flush();
    acknowledgeCompletion();
    expect(hasLiveThread()).toBe(false);

    vi.mocked(api.chatStream).mockReturnValue(stream([]) as never);
    sendTurn({ text: "two" });
    await flush();
    expect(hasLiveThread()).toBe(true);
  });
});

// ── A conversation that moved on ──────────────────────────────────────────────

describe("a run the conversation has left behind", () => {
  it("writes nothing into the conversation that replaced it", async () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValue(held.gen as never);

    sendTurn({ text: "old question" });
    await held.push({ type: "text", content: "old ans" } as ChatEvent);

    resetConversation();
    expect(getChatRun().messages).toEqual([]);

    await held.push({ type: "text", content: "wer" } as ChatEvent);
    await held.end();

    expect(getChatRun().messages).toEqual([]);
    expect(getChatRun().busy).toBe(false);
  });
});

// ── Object URLs ───────────────────────────────────────────────────────────────

describe("image previews", () => {
  it("revokes only the previews it created", () => {
    const revoke = vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
    vi.mocked(api.chatStream).mockReturnValue(deferredStream().gen as never);

    sendTurn({
      text: "what is this",
      images: [{ data: "AAA", mime_type: "image/png" }],
      previewUrls: ["blob:pond/one"],
    });
    // A history image on the same transcript: an ordinary http URL that this
    // store did not make and must not claim to free.
    patchMessage(getChatRun().messages[1].id, {
      images: ["/api/v1/sessions/s/attachments/a1"],
    });

    resetConversation();

    expect(revoke).toHaveBeenCalledTimes(1);
    expect(revoke).toHaveBeenCalledWith("blob:pond/one");
    revoke.mockRestore();
  });
});

// ── Editing ───────────────────────────────────────────────────────────────────

describe("truncateFrom", () => {
  it("drops the message and everything after it", async () => {
    vi.mocked(api.chatStream).mockReturnValue(stream([
      { type: "text", content: "answer" } as ChatEvent,
    ]) as never);
    sendTurn({ text: "question" });
    await flush();

    const userId = getChatRun().messages[0].id;
    truncateFrom(userId);
    expect(getChatRun().messages).toEqual([]);
  });
});
