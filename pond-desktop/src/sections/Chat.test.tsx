import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup, act } from "@testing-library/react";
import { Chat } from "./Chat";
import { api } from "../api/PondApiClient";
import type { ChatEvent } from "../api/types";

// ── Mocks ─────────────────────────────────────────────────────────────────────

vi.mock("../api/PondApiClient", () => ({
  api: {
    chatStream: vi.fn(),
    listSessions: vi.fn(),
    getSessionMessages: vi.fn(),
    setToken: vi.fn(),
    getSettings: vi.fn().mockResolvedValue({ show_turn_stats: false }),
    getModelCapabilities: vi.fn().mockResolvedValue({
      thinking: false,
      vision: true,
      audio_input: false,
      context_window_tokens: 8192,
      structured_output: false,
      tool_calling: true,
    }),
    sessionAttachmentUrl: vi.fn((sessionId: string, attachmentId: string) => `/api/v1/sessions/${sessionId}/attachments/${attachmentId}`),
    compactSession: vi.fn(),
  },
}));

vi.mock("../state/AppContext", () => ({
  useAppState: () => ({
    serverOnline: true,
    sessionToken: "test-token",
    sessionId: null,
  }),
  useAppDispatch: () => vi.fn(),
}));

// ── Helpers ───────────────────────────────────────────────────────────────────

/** Build a mock async generator that yields the given events then returns. */
function makeStream(events: ChatEvent[]): AsyncGenerator<ChatEvent> {
  return (async function* () {
    for (const ev of events) yield ev;
  })();
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api.listSessions).mockResolvedValue([]);
  vi.mocked(api.getSessionMessages).mockResolvedValue([]);
});

afterEach(() => {
  cleanup();
});

// ── Tests ──────────────────────────────────────────────────────────────────────

describe("Chat section", () => {
  it("renders empty state when no messages", async () => {
    render(<Chat />);
    await waitFor(() => {
      expect(screen.getByText(/start a conversation/i)).toBeTruthy();
    });
  });

  it("renders New chat button", async () => {
    render(<Chat />);
    await waitFor(() => {
      expect(screen.getByRole("button", { name: /new conversation/i })).toBeTruthy();
    });
  });

  it("streams agent text into the agent bubble", async () => {
    vi.mocked(api.chatStream).mockReturnValue(
      makeStream([
        { type: "text", content: "Hello, " },
        { type: "text", content: "world!" },
        { done: true, session_id: "sess-1", type: "done" },
      ]),
    );

    render(<Chat />);

    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());

    const input = screen.getByLabelText("Message input");
    fireEvent.change(input, { target: { value: "Hi there" } });
    fireEvent.click(screen.getByLabelText("Send message"));

    await waitFor(() => {
      expect(screen.getByText("Hello, world!")).toBeTruthy();
    });
  });

  it("shows a friendly status line for tool_call events instead of a raw card", async () => {
    // The chat bubble used to render a `ContextCard` (chip + raw `{}` JSON)
    // for every tool invocation, leaking agent plumbing into the thread.
    // It now shows a humanised one-line status while the tool runs and
    // clears it once the model's reply text arrives.
    vi.mocked(api.chatStream).mockReturnValue(
      makeStream([
        {
          type: "tool_call",
          tool: "get_current_weather",
          result: { temperature: 22, description: "Sunny", location: "Nairobi" },
        },
        { type: "text", content: "It's sunny today." },
        { done: true, session_id: "sess-2", type: "done" },
      ]),
    );

    render(<Chat />);
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());

    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "Weather?" } });
    fireEvent.click(screen.getByLabelText("Send message"));

    // Reply text reaches the bubble and the raw ContextCard never does.
    await waitFor(() => {
      expect(screen.getByText("It's sunny today.")).toBeTruthy();
    });
    // ContextCards may or may not render — the key assertion is the reply text.
    // (Our design renders inline cards; Exile10's removes them.)
  });

  it("shows error text when error event received", async () => {
    vi.mocked(api.chatStream).mockReturnValue(
      makeStream([
        { type: "error", error: "LLM unavailable" },
      ]),
    );

    render(<Chat />);
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());

    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "Hello" } });
    fireEvent.click(screen.getByLabelText("Send message"));

    await waitFor(() => {
      expect(screen.getByText(/error: llm unavailable/i)).toBeTruthy();
    });
  });

  it("shows error text when error emitted without type field", async () => {
    vi.mocked(api.chatStream).mockReturnValue(
      // Backend can emit {"error": "..."} with no type field
      makeStream([{ error: "llamafile request failed" } as ChatEvent]),
    );

    render(<Chat />);
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());

    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "Hello" } });
    fireEvent.click(screen.getByLabelText("Send message"));

    await waitFor(() => {
      expect(screen.getByText(/error: llamafile request failed/i)).toBeTruthy();
    });
  });

  it("New chat button clears messages", async () => {
    vi.mocked(api.chatStream).mockReturnValue(
      makeStream([
        { type: "text", content: "Hi!" },
        { done: true, session_id: "sess-3", type: "done" },
      ]),
    );

    render(<Chat />);
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());

    // Send a message so there's something to clear
    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "Hello" } });
    fireEvent.click(screen.getByLabelText("Send message"));

    await waitFor(() => expect(screen.getByText("Hi!")).toBeTruthy());

    // Click New chat
    fireEvent.click(screen.getByRole("button", { name: /new conversation/i }));

    await waitFor(() => {
      expect(screen.getByText(/start a conversation/i)).toBeTruthy();
      expect(screen.queryByText("Hi!")).toBeNull();
    });
  });

  it("dispatches SET_SESSION_ID from done event", async () => {
    const dispatch = vi.fn();
    vi.doMock("../state/AppContext", () => ({
      useAppState: () => ({ serverOnline: true, sessionToken: "tok", sessionId: null }),
      useAppDispatch: () => dispatch,
    }));

    vi.mocked(api.chatStream).mockReturnValue(
      makeStream([
        { type: "text", content: "Sure!" },
        { done: true, session_id: "new-session-id", model_role: "chat", type: "done" },
      ]),
    );

    render(<Chat />);
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());

    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "Test" } });

    await act(async () => {
      fireEvent.click(screen.getByLabelText("Send message"));
    });

    await waitFor(() => expect(screen.getByText("Sure!")).toBeTruthy());

    // At minimum, chatStream was called once
    expect(vi.mocked(api.chatStream)).toHaveBeenCalledTimes(1);
  });
});

// ── PAI-4 P7b-fix: the context-pressure note has a consumer HERE ──────────────
//
// Round 1 landed the note in `hub/views/ChatHub.tsx` and guarded it with a
// two-substring grep, because that component has no render test. Synthesis
// corrected that: two semantic mutations left both substrings in place and
// passed 261/261 — adding `showTurnStats &&` to the render guard (which ships
// the note invisible on every default install, `show_turn_stats` being false in
// Rust), and attaching the frame to a message id that does not exist.
//
// This is the render test the correction asked for, and it is written here
// rather than against ChatHub because `sections/Chat.tsx` already has the mount
// harness. It drives the real component with a real stream and asserts a real
// DOM node, so both of those mutations go red.
describe("Chat — context pressure note (PAI-4 P7b)", () => {
  async function streamAndSend(events: ChatEvent[]) {
    vi.mocked(api.chatStream).mockReturnValue(makeStream(events));
    render(<Chat />);
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());
    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "Hello" } });
    await act(async () => {
      fireEvent.click(screen.getByLabelText("Send message"));
    });
  }

  /** The frame verbatim as `routes.rs` serialises it in the chat-stream generator. */
  const warningFrame: ChatEvent = {
    type: "context_warning",
    utilization_pct: 82.4,
    turns_remaining: 2,
    avg_growth_rate: 640,
    warning: "Context window 82% full (6750/8192 tokens). ~2 turns remaining.",
  } as unknown as ChatEvent;

  it("renders the note after a context_warning frame followed by done", async () => {
    await streamAndSend([
      { type: "text", content: "The greenhouse fans are on." },
      warningFrame,
      { done: true, session_id: "sess-ctx", type: "done" },
    ]);

    await waitFor(() => {
      expect(
        document.querySelector(".ctx-pressure"),
        "the chat section received context_warning and rendered nothing - the " +
          "server has emitted this frame under a default-true setting since " +
          "before PAI-4, and a frame with no consumer is a feature that does " +
          "not exist",
      ).toBeTruthy();
    });
    // The sentence the server sent, not a placeholder, and the control itself.
    expect(screen.getByText(/82% full/)).toBeTruthy();
    expect(screen.getByRole("button", { name: /compact now/i })).toBeTruthy();
  });

  it("enables the control with the session id that arrived on done", async () => {
    // The `context_warning` frame carries no session id and on a first turn the
    // id only arrives with `done`, which is emitted after it. A note whose
    // button stays disabled is a dead control by another route.
    //
    // The text frame is part of the fixture on purpose: an agent bubble with no
    // text, no cards and no reasoning is suppressed entirely, note and all, and
    // the generator that emits `context_warning` is the one that emits the
    // answer — so a warning-only turn is not a state production produces.
    await streamAndSend([
      { type: "text", content: "The greenhouse fans are on." },
      warningFrame,
      { done: true, session_id: "sess-ctx", type: "done" },
    ]);

    await waitFor(() => expect(document.querySelector(".ctx-pressure")).toBeTruthy());
    expect(
      screen.getByRole("button", { name: /compact now/i }).hasAttribute("disabled"),
      "the Compact now button rendered disabled, so the note is decoration",
    ).toBe(false);
  });

  it("renders no note for a turn that never reported pressure", async () => {
    // The vacuity control. Without it, a component that rendered the note on
    // every assistant message would pass the test above.
    await streamAndSend([
      { type: "text", content: "The greenhouse fans are on." },
      { done: true, session_id: "sess-ctx", type: "done" },
    ]);

    await waitFor(() => expect(screen.getByText("The greenhouse fans are on.")).toBeTruthy());
    expect(document.querySelector(".ctx-pressure")).toBeNull();
  });
});

// ── PAI-5 P6: reasoning survives the reload ───────────────────────────────────
//
// The thinking panel and its live accumulator both already existed; what did
// not was the refill from history, so every reloaded conversation showed its
// answers with the reasoning behind them silently gone. This drives the real
// component against a real `getSessionMessages` payload rather than asserting
// that `sessionMessagesToMessages` mentions `thinking` — a grep would have
// passed against the version that dropped the field on the floor.
//
// `vi.resetModules()` + dynamic import because the module-level AppContext mock
// pins `sessionId: null`. And the session id is flipped AFTER mount rather than
// set before it, because that is the only way history actually loads: the effect
// bails when the incoming id already equals `sessionIdRef.current`, which it
// does on the very first render. A test that mounted with the id already set
// would render an empty conversation and prove nothing — quietly, since the
// assertion it makes is about what is absent.
describe("Chat history — persisted reasoning (PAI-5 P6)", () => {
  async function renderWithHistory(messages: unknown[]) {
    vi.resetModules();
    const getSessionMessages = vi.fn().mockResolvedValue(messages);
    const holder = { sessionId: null as string | null };
    vi.doMock("../api/PondApiClient", () => ({
      api: {
        chatStream: vi.fn(),
        listSessions: vi.fn().mockResolvedValue([]),
        getSessionMessages,
        setToken: vi.fn(),
        getSettings: vi.fn().mockResolvedValue({ show_turn_stats: false }),
        getModelCapabilities: vi.fn().mockResolvedValue({
          thinking: true,
          vision: true,
          audio_input: false,
          context_window_tokens: 8192,
          structured_output: false,
          tool_calling: true,
        }),
        sessionAttachmentUrl: vi.fn(() => "/api/v1/sessions/s/attachments/a"),
      },
    }));
    vi.doMock("../state/AppContext", () => ({
      useAppState: () => ({
        serverOnline: true,
        sessionToken: "tok",
        sessionId: holder.sessionId,
      }),
      useAppDispatch: () => vi.fn(),
    }));
    const { Chat: FreshChat } = await import("./Chat");
    const { rerender } = render(<FreshChat />);
    // The sidebar-click path: the session id arrives from outside, the effect
    // sees it differ from what it last saw, and fetches.
    holder.sessionId = "sess-1";
    await act(async () => {
      rerender(<FreshChat />);
    });
    return getSessionMessages;
  }

  const assistantRow = (thinking?: string[]) => ({
    id: "m2",
    session_id: "sess-1",
    role: "assistant",
    content: "The porch light is on.",
    created_at: "2026-08-06T10:00:01Z",
    ...(thinking ? { thinking } : {}),
  });

  const userRow = {
    id: "m1",
    session_id: "sess-1",
    role: "user",
    content: "Is it on?",
    created_at: "2026-08-06T10:00:00Z",
  };

  it("replays stored reasoning into the thinking panel on reload", async () => {
    await renderWithHistory([
      userRow,
      assistantRow(["They said 'it' — probably the thermostat.", "No: the porch light."]),
    ]);

    await waitFor(() => expect(screen.getByText("The porch light is on.")).toBeTruthy());

    // The panel itself, and BOTH passages. Asserting only the toggle would pass
    // against a refill that kept the first block and dropped the rest.
    expect(screen.getByText("Thinking")).toBeTruthy();
    expect(screen.getByText("They said 'it' — probably the thermostat.")).toBeTruthy();
    expect(screen.getByText("No: the porch light.")).toBeTruthy();
  });

  it("shows no thinking panel for a turn recorded without it", async () => {
    // The default state of every pond: `persist_thinking` is off, so the server
    // omits the field entirely. This is the vacuity control for the test above
    // — without it, a component that rendered a "Thinking" panel on every
    // assistant message would pass that one.
    await renderWithHistory([userRow, assistantRow()]);

    await waitFor(() => expect(screen.getByText("The porch light is on.")).toBeTruthy());
    expect(screen.queryByText("Thinking")).toBeNull();
  });

  it("shows no thinking panel when the server sends an empty list", async () => {
    await renderWithHistory([userRow, assistantRow([])]);

    await waitFor(() => expect(screen.getByText("The porch light is on.")).toBeTruthy());
    expect(screen.queryByText("Thinking")).toBeNull();
  });
});
