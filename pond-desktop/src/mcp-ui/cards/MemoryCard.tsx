import { BrainCircuit, Bookmark, Trash2, CheckCircle, Loader } from "lucide-react";
import { Chip } from "@heroui/react";
import { registerMcpCard, type McpCardProps } from "../registry";

interface MemoryFragment {
  content: string;
  segment: string;
  importance: number;
  created_at?: string;
}

const SEGMENT_COLORS: Record<string, string> = {
  identity: "#7C3AED",
  preference: "#0072F5",
  correction: "#EF4444",
  relationship: "#16A34A",
  project: "#F59E0B",
  knowledge: "#0891B2",
  context: "#8A8A8A",
};

function importanceColor(importance: number): string {
  if (importance >= 0.8) return "#EF4444";
  if (importance >= 0.6) return "#F59E0B";
  if (importance >= 0.4) return "#0072F5";
  return "#B5B5B5";
}

function relativeTime(dateStr?: string): string {
  if (!dateStr) return "";
  const d = new Date(dateStr);
  if (isNaN(d.getTime())) return dateStr;
  const diffMs = Date.now() - d.getTime();
  const diffSec = Math.floor(diffMs / 1000);
  if (diffSec < 60) return "just now";
  const diffMin = Math.floor(diffSec / 60);
  if (diffMin < 60) return `${diffMin}m ago`;
  const diffHr = Math.floor(diffMin / 60);
  if (diffHr < 24) return `${diffHr}h ago`;
  const diffDay = Math.floor(diffHr / 24);
  return `${diffDay}d ago`;
}

function MemoryCard({ data, variant }: McpCardProps) {
  const isCompact = variant === "compact";
  const memories = (data.memories ?? []) as MemoryFragment[];
  const saved = data.saved as boolean | undefined;
  const content = data.content as string | undefined;
  const segment = data.segment as string | undefined;
  const removed = data.removed as boolean | undefined;

  // Loading state
  if (memories.length === 0 && saved == null && removed == null && !content) {
    return (
      <div className="ui-card ui-memory">
        <div style={{ display: "flex", alignItems: "center", gap: 8, padding: "8px 0" }}>
          <Loader size={16} style={{ animation: "spin 1.5s linear infinite", color: "#8C4BFF" }} />
          <span style={{ fontSize: 13, color: "#8A8A8A" }}>Loading memories...</span>
        </div>
        <style>{`@keyframes spin { to { transform: rotate(360deg); } }`}</style>
      </div>
    );
  }

  // save_memory result
  if (saved === true && content) {
    const seg = (segment ?? "context").toLowerCase();
    return (
      <div className="ui-card ui-memory">
        <div className="ui-memory__saved">
          <CheckCircle size={16} style={{ color: "#16A34A", flexShrink: 0 }} />
          <div className="ui-memory__saved-body">
            <span className="ui-memory__saved-label">Saved</span>
            <span className="ui-memory__saved-content">{content}</span>
          </div>
          <Chip
            size="sm"
            variant="soft"
            style={{
              background: `${SEGMENT_COLORS[seg] ?? "#8A8A8A"}18`,
              color: SEGMENT_COLORS[seg] ?? "#8A8A8A",
              textTransform: "capitalize",
              flexShrink: 0,
            }}
          >
            {seg}
          </Chip>
        </div>
      </div>
    );
  }

  // forget_memory result
  if (removed === true) {
    return (
      <div className="ui-card ui-memory">
        <div className="ui-memory__saved">
          <Trash2 size={16} style={{ color: "#EF4444", flexShrink: 0 }} />
          <span style={{ fontSize: 13, color: "#5F5F5F" }}>
            {content ? `Removed: "${content}"` : "Memory removed"}
          </span>
        </div>
      </div>
    );
  }

  // recall_memories result — list view
  const visible = memories.slice(0, isCompact ? 3 : 8);

  return (
    <div className="ui-card ui-memory">
      <div className="ui-memory__header">
        <BrainCircuit size={15} style={{ color: "#7C3AED" }} />
        <span className="ui-memory__header-title">Memories</span>
        <Chip size="sm" variant="soft" style={{ background: "#F3EFFF", color: "#7C3AED" }}>
          {memories.length}
        </Chip>
      </div>

      {visible.length === 0 ? (
        <div className="ui-memory__empty">
          <Bookmark size={20} style={{ color: "#D9D9D9" }} />
          <span>No memories stored</span>
        </div>
      ) : (
        <div className="ui-memory__list">
          {visible.map((m, i) => {
            const seg = (m.segment ?? "context").toLowerCase();
            const color = SEGMENT_COLORS[seg] ?? "#8A8A8A";
            return (
              <div key={i} className="ui-memory__item">
                <div
                  className="ui-memory__importance"
                  style={{ background: importanceColor(m.importance ?? 0.5) }}
                  title={`Importance: ${((m.importance ?? 0.5) * 100).toFixed(0)}%`}
                />
                <div className="ui-memory__body">
                  <span className="ui-memory__content">{m.content}</span>
                  <div className="ui-memory__meta-row">
                    <span
                      className="ui-memory__segment-chip"
                      style={{ background: `${color}18`, color }}
                    >
                      {seg}
                    </span>
                    {m.created_at && (
                      <span className="ui-memory__meta">{relativeTime(m.created_at)}</span>
                    )}
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

registerMcpCard({
  key: "memory",
  label: "Memory",
  icon: "BrainCircuit",
  toolPattern: /memory|recall_memor|save_memory|forget_memory/,
  component: MemoryCard,
  mockTool: "giap-memory__recall_memories",
  mockData: {
    memories: [
      { content: "Prefers concise answers without lengthy preambles", segment: "preference", importance: 0.75, created_at: new Date(Date.now() - 2 * 3600000).toISOString() },
      { content: "Name is Jerry", segment: "identity", importance: 0.9, created_at: new Date(Date.now() - 86400000).toISOString() },
      { content: "Works on the Goose in a Pond project", segment: "project", importance: 0.65, created_at: new Date(Date.now() - 3 * 86400000).toISOString() },
      { content: "Prefers Rust over Go for backend services", segment: "preference", importance: 0.7, created_at: new Date(Date.now() - 7 * 86400000).toISOString() },
      { content: "Never add Co-Authored-By in commit messages", segment: "correction", importance: 0.95, created_at: new Date(Date.now() - 10 * 86400000).toISOString() },
    ],
  },
});
