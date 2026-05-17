import { BookOpen, Search, ExternalLink, Loader } from "lucide-react";
import { registerMcpCard, type McpCardProps } from "../registry";

interface SearchResult {
  title: string;
  snippet: string;
}

function KnowledgeCard({ data, variant }: McpCardProps) {
  const isCompact = variant === "compact";
  const title = data.title as string | undefined;
  const summary = data.summary as string | undefined;
  const sourceUrl = data.source_url as string | undefined;
  const query = data.query as string | undefined;
  const results = (data.results ?? []) as SearchResult[];

  // Loading state
  if (!title && !summary && results.length === 0 && !query) {
    return (
      <div className="ui-card ui-knowledge">
        <div style={{ display: "flex", alignItems: "center", gap: 8, padding: "8px 0" }}>
          <Loader size={16} style={{ animation: "spin 1.5s linear infinite", color: "#8C4BFF" }} />
          <span style={{ fontSize: 13, color: "#8A8A8A" }}>Looking it up...</span>
        </div>
        <style>{`@keyframes spin { to { transform: rotate(360deg); } }`}</style>
      </div>
    );
  }

  // Search results view
  if (results.length > 0) {
    const visible = results.slice(0, isCompact ? 2 : 5);
    return (
      <div className="ui-card ui-knowledge">
        <div className="ui-knowledge__header">
          <Search size={14} style={{ color: "#0072F5", flexShrink: 0 }} />
          <span className="ui-knowledge__query">{query || "Search results"}</span>
        </div>
        <div className="ui-knowledge__results">
          {visible.map((r, i) => (
            <div key={i} className="ui-knowledge__result">
              <span className="ui-knowledge__result-num">{i + 1}</span>
              <div className="ui-knowledge__result-body">
                <span className="ui-knowledge__result-title">{r.title}</span>
                <span className="ui-knowledge__result-snippet">{r.snippet}</span>
              </div>
            </div>
          ))}
        </div>
      </div>
    );
  }

  // Article / define view
  const summaryText = summary ?? "";
  const displaySummary = isCompact
    ? summaryText.slice(0, 180) + (summaryText.length > 180 ? "…" : "")
    : summaryText;

  return (
    <div className="ui-card ui-knowledge">
      <div className="ui-knowledge__header">
        <BookOpen size={14} style={{ color: "#0072F5", flexShrink: 0 }} />
        {query && <span className="ui-knowledge__query">{query}</span>}
      </div>

      {title && <h3 className="ui-knowledge__title">{title}</h3>}

      {displaySummary && (
        <p className="ui-knowledge__summary">{displaySummary}</p>
      )}

      {sourceUrl && (
        <a
          href={sourceUrl}
          className="ui-knowledge__source"
          target="_blank"
          rel="noopener noreferrer"
          onClick={(e) => e.stopPropagation()}
        >
          <ExternalLink size={11} />
          <span>{sourceUrl.replace(/^https?:\/\//, "").slice(0, 50)}</span>
        </a>
      )}
    </div>
  );
}

registerMcpCard({
  key: "knowledge",
  label: "Knowledge",
  icon: "BookOpen",
  toolPattern: /wikipedia|knowledge|instant_answer|define_word|search_books/,
  component: KnowledgeCard,
  mockTool: "giap-knowledge__get_wikipedia_article",
  mockData: {
    title: "Tauri (software framework)",
    query: "Tauri desktop framework",
    summary:
      "Tauri is an open-source framework for building desktop and mobile applications using web technologies for the frontend, combined with a Rust backend. It provides a smaller binary size and memory footprint compared to Electron by using the operating system's native web renderer instead of bundling Chromium. Tauri supports Windows, macOS, and Linux, with mobile support added in version 2.0.",
    source_url: "https://en.wikipedia.org/wiki/Tauri_(software_framework)",
  },
});
