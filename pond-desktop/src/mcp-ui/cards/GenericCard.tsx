import type { McpCardProps } from "../registry";

export function GenericCard({ data }: McpCardProps) {
  const text = typeof data === "string"
    ? data
    : JSON.stringify(data ?? {}, null, 2);

  return (
    <div className="ui-card ui-generic">
      <pre className="ui-generic__pre">{text}</pre>
    </div>
  );
}

// GenericCard is NOT auto-registered — it's used as an explicit fallback
// by Canvas and ContextCard when no registered renderer matches.
