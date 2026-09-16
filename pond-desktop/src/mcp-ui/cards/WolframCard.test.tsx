import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { WolframCard } from "./WolframCard";
import { findCardByHint } from "../registry";

afterEach(() => cleanup());

const MERCURY = {
  query: "mercury",
  primary: "Mercury (planet)",
  primary_title: "Result",
  pods: [
    { title: "Orbital period", text: "87.97 days" },
    { title: "Mean radius", text: "2439.7 km" },
  ],
  explore: [
    { id: "w1", label: "a chemical element", kind: "assumption", verb: "interpret as" },
    { id: "w2", label: "Surface temperature", kind: "pod", verb: "show section" },
  ],
  source_url: "https://www.wolframalpha.com/input?i=mercury",
};

describe("WolframCard", () => {
  it("is registered under the key the server's hint uses", () => {
    // The Rust side emits `[[[mcp-ui:wolfram:{...}]]]` and pond-api turns the
    // segment before the JSON into `ui.card_type`. If these two strings ever
    // disagree the card silently stops rendering, with nothing failing.
    expect(findCardByHint("wolfram")?.component).toBe(WolframCard);
  });

  it("leads with the answer", () => {
    render(<WolframCard data={MERCURY} toolName="giap-knowledge__compute_answer" />);
    expect(screen.getByText("Mercury (planet)")).toBeTruthy();
    expect(screen.getByText("87.97 days")).toBeTruthy();
  });

  it("does not label the answer 'Result', which says nothing", () => {
    render(<WolframCard data={MERCURY} toolName="t" />);
    expect(screen.queryByText("Result")).toBeNull();
  });

  it("keeps a meaningful answer label", () => {
    render(<WolframCard data={{ ...MERCURY, primary_title: "Exact result" }} toolName="t" />);
    expect(screen.getByText("Exact result")).toBeTruthy();
  });

  it("offers each suggestion as its own control", () => {
    render(<WolframCard data={MERCURY} toolName="t" onAction={vi.fn()} />);
    expect(screen.getByRole("button", { name: /a chemical element/i })).toBeTruthy();
    expect(screen.getByRole("button", { name: /Surface temperature/i })).toBeTruthy();
  });

  it("sends the suggestion's wording, not an id nothing resolves", () => {
    const onAction = vi.fn();
    render(<WolframCard data={MERCURY} toolName="t" onAction={onAction} />);
    fireEvent.click(screen.getByRole("button", { name: /a chemical element/i }));

    const sent = onAction.mock.calls[0][0] as string;
    // The label and the original question are the whole message: the id is a
    // list key now, and sending one would ask the model to quote a handle that
    // names nothing.
    expect(sent).toContain("a chemical element");
    expect(sent).toContain("mercury");
    expect(sent).not.toContain("w1");
  });

  it("renders suggestions as plain labels where there is nothing to send to", () => {
    // Canvas has no composer, so it passes no onAction. A button that does
    // nothing when pressed is worse than a label.
    render(<WolframCard data={MERCURY} toolName="t" />);
    expect(screen.queryByRole("button", { name: /a chemical element/i })).toBeNull();
    expect(screen.getByText("a chemical element")).toBeTruthy();
  });

  it("says it is working rather than drawing an empty box", () => {
    render(<WolframCard data={{}} toolName="t" />);
    expect(screen.getByText(/Working it out/i)).toBeTruthy();
  });

  it("renders a partial payload instead of blanking", () => {
    // A hint that arrives without pods or suggestions is still an answer.
    render(<WolframCard data={{ query: "2+2", primary: "4" }} toolName="t" />);
    expect(screen.getByText("4")).toBeTruthy();
    expect(screen.queryByText(/Working it out/i)).toBeNull();
  });

  it("links out to Wolfram without carrying a key", () => {
    render(<WolframCard data={MERCURY} toolName="t" />);
    const link = screen.getByRole("link") as HTMLAnchorElement;
    expect(link.href).toContain("wolframalpha.com/input");
    // The API endpoint carries the AppID in its query string. This link is the
    // human-facing page, and it is rendered into the transcript.
    expect(link.href).not.toContain("appid");
    expect(link.rel).toContain("noopener");
  });

  it("trims to two sections in the compact variant", () => {
    const many = {
      ...MERCURY,
      pods: [1, 2, 3, 4].map((n) => ({ title: `Pod ${n}`, text: `body ${n}` })),
    };
    render(<WolframCard data={many} toolName="t" variant="compact" />);
    expect(screen.getByText("body 1")).toBeTruthy();
    expect(screen.queryByText("body 3")).toBeNull();
  });
});
