import { Sigma, ExternalLink, Loader } from "lucide-react";
import { registerMcpCard, type McpCardProps } from "../registry";

/**
 * Wolfram|Alpha computation result.
 *
 * The shape is whatever `crates/pond-mcp-server/src/wolfram.rs` puts in its
 * `[[[mcp-ui:wolfram:{...}]]]` hint, so the two move together. Everything here
 * is optional on purpose: a hint that arrives half-built should render the part
 * it has rather than blanking the card.
 */
interface Pod {
  title?: string;
  text?: string;
}

interface Suggestion {
  id?: string;
  label?: string;
  kind?: string;
  verb?: string;
}

function WolframCard({ data, onAction, variant }: McpCardProps) {
  const isCompact = variant === "compact";
  const query = data.query as string | undefined;
  const primary = data.primary as string | undefined;
  const primaryTitle = data.primary_title as string | undefined;
  const pods = (data.pods ?? []) as Pod[];
  const explore = (data.explore ?? []) as Suggestion[];
  const sourceUrl = data.source_url as string | undefined;

  // Nothing has arrived yet. The tool call is already on screen by this point,
  // so the card has to say it is working rather than render an empty box.
  if (!primary && pods.length === 0 && explore.length === 0) {
    return (
      <div className="ui-wolfram ui-wolfram--loading">
        <Loader size={14} className="ui-wolfram__spinner" aria-hidden />
        <span className="ui-wolfram__working">Working it out…</span>
      </div>
    );
  }

  const visiblePods = isCompact ? pods.slice(0, 2) : pods.slice(0, 5);

  return (
    <div className="ui-wolfram">
      <div className="ui-wolfram__header">
        <Sigma size={13} className="ui-wolfram__icon" aria-hidden />
        {query && <span className="ui-wolfram__query">{query}</span>}
      </div>

      {primary && (
        <div className="ui-wolfram__answer">
          {primaryTitle && primaryTitle.toLowerCase() !== "result" && (
            <span className="ui-wolfram__answer-label">{primaryTitle}</span>
          )}
          <span className="ui-wolfram__answer-value">{primary}</span>
        </div>
      )}

      {visiblePods.length > 0 && (
        <dl className="ui-wolfram__pods">
          {visiblePods.map((pod, i) => (
            <div key={i} className="ui-wolfram__pod">
              <dt className="ui-wolfram__pod-title">{pod.title}</dt>
              <dd className="ui-wolfram__pod-text">{pod.text}</dd>
            </div>
          ))}
        </dl>
      )}

      {explore.length > 0 && (
        <div className="ui-wolfram__explore">
          <span className="ui-wolfram__explore-label">
            {explore.length === 1 ? "Also available" : "Also available"}
          </span>
          <div className="ui-wolfram__chips" role="group" aria-label="Other readings of this question">
            {explore.map((s, i) => (
              <SuggestionChip key={s.id ?? i} suggestion={s} query={query} onAction={onAction} />
            ))}
          </div>
        </div>
      )}

      {sourceUrl && !isCompact && (
        <a
          href={sourceUrl}
          className="ui-wolfram__source"
          target="_blank"
          rel="noopener noreferrer"
          onClick={(e) => e.stopPropagation()}
        >
          <ExternalLink size={10} aria-hidden />
          <span>Wolfram|Alpha</span>
        </a>
      )}
    </div>
  );
}

/**
 * One explorable suggestion.
 *
 * Clicking it does NOT call the tool directly. It asks the assistant, which
 * re-asks `compute_answer` with the suggestion's own wording on the normal
 * engine path — so the follow-up is audited, policy-checked and part of the
 * conversation, the same as if the user had typed it. Reaching the tool from
 * here would need the knowledge tools on `DIRECT_DISPATCH_ALLOWLIST`, which
 * would hand them to every paired client and every sandboxed MCP App iframe.
 *
 * It used to send the suggestion's id for `explore_computation` to resolve.
 * That tool was removed on 2026-09-10, so the wording is now the whole message:
 * an id would name nothing.
 *
 * With no `onAction` wired (the Canvas surface, today) it renders as a plain
 * label rather than a button that does nothing when pressed.
 */
function SuggestionChip({
  suggestion,
  query,
  onAction,
}: {
  suggestion: Suggestion;
  query?: string;
  onAction?: (prompt: string) => void;
}) {
  const label = suggestion.label ?? suggestion.id ?? "";
  if (!label) return null;

  const verb = suggestion.verb ?? "open";
  const title = `${verb} ${label}`;

  if (!onAction || !suggestion.id) {
    return (
      <span className="ui-wolfram__chip ui-wolfram__chip--static" title={title}>
        {label}
      </span>
    );
  }

  // The wording is the whole message: nothing resolves an id any more, so the
  // sent text has to stand on its own as something a person would say.
  const prompt = query
    ? `For "${query}", ${verb} ${label}.`
    : `${verb.charAt(0).toUpperCase()}${verb.slice(1)} ${label}.`;

  return (
    <button
      type="button"
      className="ui-wolfram__chip"
      title={title}
      onClick={(e) => {
        e.stopPropagation();
        onAction(prompt);
      }}
    >
      {label}
    </button>
  );
}

registerMcpCard({
  key: "wolfram",
  label: "Computed",
  icon: "Sigma",
  toolPattern: /compute_answer|wolfram/,
  component: WolframCard,
  mockTool: "giap-knowledge__compute_answer",
  mockData: {
    query: "mercury",
    primary: "Mercury (planet)",
    primary_title: "Result",
    pods: [
      { title: "Orbital period", text: "87.97 days" },
      { title: "Mean radius", text: "2439.7 km" },
    ],
    explore: [
      { id: "w1", label: "a chemical element", kind: "assumption", verb: "interpret as" },
      { id: "w2", label: "a Roman god", kind: "assumption", verb: "interpret as" },
      { id: "w3", label: "Surface temperature", kind: "pod", verb: "show section" },
    ],
    source_url: "https://www.wolframalpha.com/input?i=mercury",
  },
});

export { WolframCard };
