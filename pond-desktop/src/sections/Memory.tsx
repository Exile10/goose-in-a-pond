import { useState, useEffect, useRef } from "react";
import { Card, CardContent, Button, Chip } from "@heroui/react";
import {
  BrainCircuit,
  Shield,
  Heart,
  Wrench,
  FolderOpen,
  BookOpen,
  Clock,
  Star,
  Trash2,
  Plus,
  RefreshCw,
  X,
  Search,
  Pencil,
  Save,
  GitMerge,
  MessageSquare,
  Layers,
  Radio,
  Sparkles,
  Brain,
} from "lucide-react";
import { api } from "../api/PondApiClient";
import { PageHeader, useConfirm, SkeletonList } from "../components/shared";
import type { MemoryFragment, MemorySegment, MemoryTier, Settings } from "../api/types";

// ── Segment metadata ──────────────────────────────────────────

type SegmentKey = MemorySegment | "all";
type TierKey = MemoryTier | "all";
type SourceKey = "auto" | "mcp" | "chat" | "all";

const SEGMENTS: Record<
  MemorySegment,
  { label: string; icon: React.ReactNode; cssClass: string; importanceDefault: number }
> = {
  identity:     { label: "Identity",     icon: <Shield size={13} strokeWidth={1.8} />,    cssClass: "identity",     importanceDefault: 0.8 },
  preference:   { label: "Preference",   icon: <Heart size={13} strokeWidth={1.8} />,     cssClass: "preference",   importanceDefault: 0.7 },
  correction:   { label: "Correction",   icon: <Wrench size={13} strokeWidth={1.8} />,    cssClass: "correction",   importanceDefault: 0.9 },
  relationship: { label: "Relationship", icon: <Heart size={13} strokeWidth={1.8} />,     cssClass: "relationship", importanceDefault: 0.7 },
  project:      { label: "Project",      icon: <FolderOpen size={13} strokeWidth={1.8} />,cssClass: "project",      importanceDefault: 0.6 },
  knowledge:    { label: "Knowledge",    icon: <BookOpen size={13} strokeWidth={1.8} />,  cssClass: "knowledge",    importanceDefault: 0.5 },
  context:      { label: "Context",      icon: <Clock size={13} strokeWidth={1.8} />,     cssClass: "context",      importanceDefault: 0.3 },
};

const SEGMENT_IMPORT_FILL: Record<MemorySegment, string> = {
  identity:     "var(--mem-identity)",
  preference:   "var(--mem-preference)",
  correction:   "var(--mem-correction)",
  relationship: "var(--mem-relationship)",
  project:      "var(--mem-project)",
  knowledge:    "var(--mem-knowledge)",
  context:      "var(--mem-context)",
};

const SEGMENT_ORDER: MemorySegment[] = [
  "identity", "preference", "correction", "relationship", "project", "knowledge", "context",
];

// ── Helpers ───────────────────────────────────────────────────

function relativeTime(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime();
  const mins = Math.floor(diff / 60_000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  const days = Math.floor(hrs / 24);
  if (days < 30) return `${days}d ago`;
  return new Date(iso).toLocaleDateString();
}

function sourceLabel(source?: string): string {
  if (!source) return "";
  if (source === "extraction") return "auto";
  if (source === "mcp_tool") return "mcp";
  if (source === "chat") return "chat";
  if (source === "note") return "note";
  return source;
}

function normalizedSource(source?: string): SourceKey {
  const label = sourceLabel(source);
  if (label === "auto") return "auto";
  if (label === "mcp") return "mcp";
  if (label === "chat") return "chat";
  return "auto";
}

// ── Stats bar ─────────────────────────────────────────────────

function MemStatsBar({ items }: { items: MemoryFragment[] }) {
  const counts = SEGMENT_ORDER.reduce<Partial<Record<MemorySegment, number>>>((acc, seg) => {
    const n = items.filter((m) => m.segment === seg).length;
    if (n > 0) acc[seg] = n;
    return acc;
  }, {});

  if (Object.keys(counts).length === 0) return null;

  return (
    <div className="mem-stats">
      {SEGMENT_ORDER.filter((s) => counts[s]).map((seg) => (
        <span
          key={seg}
          className={`mem-stats__chip mem-seg-badge--${seg}`}
          title={SEGMENTS[seg].label}
        >
          <span className="mem-stats__dot" style={{ background: SEGMENT_IMPORT_FILL[seg] }} />
          {counts[seg]} {SEGMENTS[seg].label}
        </span>
      ))}
    </div>
  );
}

// ── Filter row ────────────────────────────────────────────────

function MemFilterRow({
  activeSegment,
  activeTier,
  activeSource,
  segCounts,
  tierCounts,
  sourceCounts,
  total,
  onSegmentChange,
  onTierChange,
  onSourceChange,
}: {
  activeSegment: SegmentKey;
  activeTier: TierKey;
  activeSource: SourceKey;
  segCounts: Partial<Record<MemorySegment, number>>;
  tierCounts: Partial<Record<MemoryTier, number>>;
  sourceCounts: Partial<Record<SourceKey, number>>;
  total: number;
  onSegmentChange: (seg: SegmentKey) => void;
  onTierChange: (tier: TierKey) => void;
  onSourceChange: (source: SourceKey) => void;
}) {
  const tierLabels: Record<MemoryTier, string> = {
    short: "Short",
    long: "Long",
    permanent: "Permanent",
  };

  const sourceLabels: Record<SourceKey, string> = {
    all: "All Sources",
    auto: "Auto",
    mcp: "MCP",
    chat: "Chat",
  };

  const hasTiers = Object.keys(tierCounts).length > 0;
  const hasSources = Object.keys(sourceCounts).filter((k) => k !== "all").length > 0;

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
      {/* Segment filter */}
      <div className="mem-filter">
        <button
          className={`mem-filter__btn${activeSegment === "all" ? " is-active" : ""}`}
          onClick={() => onSegmentChange("all")}
        >
          All
          <span className="mem-filter__count">{total}</span>
        </button>
        {SEGMENT_ORDER.filter((s) => segCounts[s]).map((seg) => (
          <button
            key={seg}
            className={`mem-filter__btn${activeSegment === seg ? " is-active" : ""}`}
            onClick={() => onSegmentChange(seg)}
          >
            {SEGMENTS[seg].label}
            <span className="mem-filter__count">{segCounts[seg]}</span>
          </button>
        ))}
      </div>

      {/* Tier + Source secondary filters */}
      {(hasTiers || hasSources) && (
        <div style={{ display: "flex", gap: 6, flexWrap: "wrap" as React.CSSProperties["flexWrap"], alignItems: "center" }}>
          {hasTiers && (
            <div style={{ display: "flex", gap: 4, alignItems: "center" }}>
              <span style={{ fontSize: "10px", fontWeight: 600, letterSpacing: "0.07em", color: "var(--grey-400)", textTransform: "uppercase" as React.CSSProperties["textTransform"], marginRight: 2 }}>
                <Layers size={10} strokeWidth={2} style={{ display: "inline", verticalAlign: "middle", marginRight: 2 }} />
                Tier
              </span>
              {(["all", "short", "long", "permanent"] as const).map((tier) => {
                const count = tier === "all" ? total : tierCounts[tier];
                if (tier !== "all" && !count) return null;
                return (
                  <button
                    key={tier}
                    className={`mem-filter__btn${activeTier === tier ? " is-active" : ""}`}
                    style={{ fontSize: "11px", padding: "2px 8px", height: "22px" }}
                    onClick={() => onTierChange(tier)}
                  >
                    {tier === "all" ? "Any" : tierLabels[tier]}
                    {tier !== "all" && count !== undefined && (
                      <span className="mem-filter__count">{count}</span>
                    )}
                  </button>
                );
              })}
            </div>
          )}

          {hasTiers && hasSources && (
            <span style={{ width: 1, height: 16, background: "var(--grey-150)", display: "inline-block" }} />
          )}

          {hasSources && (
            <div style={{ display: "flex", gap: 4, alignItems: "center" }}>
              <span style={{ fontSize: "10px", fontWeight: 600, letterSpacing: "0.07em", color: "var(--grey-400)", textTransform: "uppercase" as React.CSSProperties["textTransform"], marginRight: 2 }}>
                <Radio size={10} strokeWidth={2} style={{ display: "inline", verticalAlign: "middle", marginRight: 2 }} />
                Source
              </span>
              {(["all", "auto", "mcp", "chat"] as const).map((src) => {
                const count = src === "all" ? total : sourceCounts[src];
                if (src !== "all" && !count) return null;
                return (
                  <button
                    key={src}
                    className={`mem-filter__btn${activeSource === src ? " is-active" : ""}`}
                    style={{ fontSize: "11px", padding: "2px 8px", height: "22px" }}
                    onClick={() => onSourceChange(src)}
                  >
                    {sourceLabels[src]}
                    {src !== "all" && count !== undefined && (
                      <span className="mem-filter__count">{count}</span>
                    )}
                  </button>
                );
              })}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

// ── Memory row (design-spec layout) ──────────────────────────

function MemRow({
  mem,
  onDelete,
  onUpdate,
}: {
  mem: MemoryFragment;
  onDelete: (id: string) => void;
  onUpdate: (id: string, newContent: string) => Promise<void>;
}) {
  const [editing, setEditing] = useState(false);
  const [editContent, setEditContent] = useState(mem.content);
  const [saving, setSaving] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const seg = mem.segment;
  const segMeta = seg ? SEGMENTS[seg] : null;
  const segClass = seg ? seg : "none";
  const importanceFill = seg ? SEGMENT_IMPORT_FILL[seg] : "var(--grey-300)";
  const importance = mem.importance ?? (seg ? segMeta?.importanceDefault : undefined);

  function startEdit() {
    setEditContent(mem.content);
    setEditing(true);
    setTimeout(() => textareaRef.current?.focus(), 0);
  }

  function cancelEdit() {
    setEditing(false);
    setEditContent(mem.content);
  }

  async function handleSave() {
    const trimmed = editContent.trim();
    if (!trimmed || trimmed === mem.content) {
      cancelEdit();
      return;
    }
    setSaving(true);
    try {
      await onUpdate(mem.id, trimmed);
      setEditing(false);
    } finally {
      setSaving(false);
    }
  }

  if (editing) {
    // Editing state — spans full row width
    return (
      <div className="mem-row" style={{ display: "block", padding: "12px" }}>
        <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          <textarea
            ref={textareaRef}
            value={editContent}
            onChange={(e) => setEditContent(e.target.value)}
            disabled={saving}
            rows={3}
            style={{
              width: "100%",
              padding: "8px 12px",
              border: "1px solid var(--color-accent)",
              borderRadius: "8px",
              fontSize: "13px",
              fontFamily: "var(--font-body)",
              background: "var(--color-accent-subtle)",
              color: "var(--fg)",
              outline: "none",
              resize: "vertical" as React.CSSProperties["resize"],
              lineHeight: 1.6,
              boxSizing: "border-box" as React.CSSProperties["boxSizing"],
              boxShadow: "0 0 0 3px var(--color-accent-subtle)",
            }}
            aria-label="Edit memory content"
          />
          <div style={{ display: "flex", gap: 6 }}>
            <button
              onClick={handleSave}
              disabled={saving || !editContent.trim()}
              style={{
                display: "flex",
                alignItems: "center",
                gap: 5,
                height: "28px",
                padding: "0 12px",
                border: "none",
                borderRadius: "7px",
                background: "var(--color-accent)",
                color: "#fff",
                fontSize: "12px",
                fontWeight: 600,
                cursor: saving ? "wait" : "pointer",
                opacity: saving ? 0.7 : 1,
                fontFamily: "var(--font-body)",
                transition: "opacity 120ms",
              }}
              aria-label="Save edit"
            >
              <Save size={12} strokeWidth={2} />
              {saving ? "Saving…" : "Save"}
            </button>
            <button
              onClick={cancelEdit}
              disabled={saving}
              style={{
                display: "flex",
                alignItems: "center",
                gap: 5,
                height: "28px",
                padding: "0 12px",
                border: "1px solid var(--grey-200)",
                borderRadius: "7px",
                background: "transparent",
                color: "var(--grey-600)",
                fontSize: "12px",
                fontWeight: 500,
                cursor: "pointer",
                fontFamily: "var(--font-body)",
              }}
              aria-label="Cancel edit"
            >
              Cancel
            </button>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="mem-row">
      {/* Bullet: segment icon bubble */}
      <div className={`mem-row__bullet mem-card__seg-icon--${segClass}`}>
        {segMeta ? segMeta.icon : <Sparkles size={12} strokeWidth={1.8} />}
      </div>

      {/* Main text + meta */}
      <div className="mem-row__text">
        <div>{mem.content}</div>
        {/* Badges + importance line */}
        <div style={{ display: "flex", alignItems: "center", gap: 6, marginTop: 4, flexWrap: "wrap" as React.CSSProperties["flexWrap"] }}>
          {seg && (
            <span className={`mem-seg-badge mem-seg-badge--${segClass}`}>
              {segMeta?.icon}
              {segMeta?.label ?? seg}
            </span>
          )}
          {mem.tier && (
            <span className={`mem-tier-badge mem-tier-badge--${mem.tier}`}>
              {mem.tier}
            </span>
          )}
          {mem.source && (
            <span className="mem-source-badge">{sourceLabel(mem.source)}</span>
          )}
          {importance !== undefined && (
            <span className="mem-card__meta-item mem-card__meta-item--importance">
              <span className="mem-importance__track">
                <span
                  className="mem-importance__fill"
                  style={{
                    width: `${Math.round(importance * 100)}%`,
                    background: importanceFill,
                  }}
                />
              </span>
              <span className="mem-importance__label">{Math.round(importance * 10) / 10}</span>
            </span>
          )}
          {mem.access_count > 0 && (
            <span className="mem-card__meta-item">
              <Star size={10} strokeWidth={1.8} />
              {mem.access_count}x
            </span>
          )}
          {mem.tags && mem.tags.length > 0 && (
            <span className="mem-card__meta-item">
              {mem.tags.slice(0, 3).join(", ")}
            </span>
          )}
        </div>
      </div>

      {/* Date */}
      <div className="mem-row__date">
        <Clock size={10} strokeWidth={1.8} style={{ display: "inline", marginRight: 3, verticalAlign: "middle" }} />
        {relativeTime(mem.created_at)}
      </div>

      {/* Actions */}
      <div style={{ display: "flex", gap: 2 }}>
        <button
          className="mem-card__delete"
          onClick={startEdit}
          aria-label="Edit memory"
          title="Edit memory"
        >
          <Pencil size={12} strokeWidth={1.8} />
        </button>
        <button
          className="mem-card__delete"
          onClick={() => onDelete(mem.id)}
          aria-label="Delete memory"
          title="Delete memory"
        >
          <Trash2 size={12} strokeWidth={1.8} />
        </button>
      </div>
    </div>
  );
}

// ── Add memory modal ──────────────────────────────────────────

function AddMemoryModal({
  onAdd,
  onClose,
}: {
  onAdd: (
    content: string,
    segment?: MemorySegment,
    importance?: number,
    tier?: MemoryTier,
  ) => Promise<void>;
  onClose: () => void;
}) {
  const [content, setContent] = useState("");
  const [saving, setSaving] = useState(false);
  const [segment, setSegment] = useState<MemorySegment | "">("");
  const [importance, setImportance] = useState(0.7);
  const [tier, setTier] = useState<MemoryTier | "">("");
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    textareaRef.current?.focus();
  }, []);

  useEffect(() => {
    function handleKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
  }, [onClose]);

  async function handleSave() {
    const trimmed = content.trim();
    if (!trimmed) return;
    setSaving(true);
    try {
      await onAdd(
        trimmed,
        segment !== "" ? (segment as MemorySegment) : undefined,
        segment !== "" ? importance : undefined,
        tier !== "" ? (tier as MemoryTier) : undefined,
      );
      onClose();
    } finally {
      setSaving(false);
    }
  }

  const segmentColor = segment !== "" ? SEGMENT_IMPORT_FILL[segment as MemorySegment] : undefined;

  return (
    <div style={modalStyles.overlay} onClick={onClose}>
      <div style={modalStyles.dialog} onClick={(e) => e.stopPropagation()} role="dialog" aria-modal="true" aria-label="Add memory">
        {/* Header */}
        <div style={modalStyles.header}>
          <div style={modalStyles.headerLeft}>
            <div style={modalStyles.headerIcon}>
              <BrainCircuit size={16} strokeWidth={1.8} />
            </div>
            <h2 style={modalStyles.title}>Add Memory</h2>
          </div>
          <button style={modalStyles.closeBtn} onClick={onClose} aria-label="Close">
            <X size={16} strokeWidth={1.8} />
          </button>
        </div>

        <div style={modalStyles.divider} />

        {/* Body */}
        <div style={modalStyles.body}>
          {/* Content textarea */}
          <div style={modalStyles.fieldGroup}>
            <label style={modalStyles.label} htmlFor="mem-content">Content</label>
            <textarea
              id="mem-content"
              ref={textareaRef}
              style={modalStyles.textarea}
              placeholder="What should the assistant remember?"
              value={content}
              onChange={(e) => setContent(e.target.value)}
              disabled={saving}
              rows={4}
              aria-label="Memory content"
            />
          </div>

          {/* Segment dropdown */}
          <div style={modalStyles.fieldGroup}>
            <label style={modalStyles.label} htmlFor="mem-segment">Segment</label>
            <div style={modalStyles.selectWrap}>
              {segment !== "" && (
                <span style={{ ...modalStyles.segDot, background: segmentColor }} />
              )}
              <select
                id="mem-segment"
                style={{
                  ...modalStyles.select,
                  paddingLeft: segment !== "" ? "28px" : "10px",
                }}
                value={segment}
                onChange={(e) => {
                  const val = e.target.value as MemorySegment | "";
                  setSegment(val);
                  if (val !== "") {
                    setImportance(SEGMENTS[val as MemorySegment].importanceDefault);
                  }
                }}
                disabled={saving}
              >
                <option value="">Auto-classify</option>
                {SEGMENT_ORDER.map((s) => (
                  <option key={s} value={s}>{SEGMENTS[s].label}</option>
                ))}
              </select>
            </div>
          </div>

          {/* Importance slider */}
          <div style={modalStyles.fieldGroup}>
            <div style={modalStyles.importanceLabelRow}>
              <label style={modalStyles.label} htmlFor="mem-importance">Importance</label>
              <span style={modalStyles.importanceVal}>
                {segment !== "" ? importance.toFixed(2) : "–"}
              </span>
            </div>
            <input
              id="mem-importance"
              type="range"
              style={{
                ...modalStyles.range,
                accentColor: segmentColor ?? "var(--color-accent)",
                opacity: segment === "" ? 0.4 : 1,
                cursor: segment === "" ? "not-allowed" : "pointer",
              }}
              min={0}
              max={1}
              step={0.05}
              value={importance}
              onChange={(e) => setImportance(parseFloat(e.target.value))}
              disabled={saving || segment === ""}
              title={segment === "" ? "Select a segment first to set importance" : `Importance: ${importance}`}
            />
            <div style={modalStyles.rangeLabels}>
              <span>Low</span>
              <span>High</span>
            </div>
          </div>

          {/* Tier selector */}
          <div style={modalStyles.fieldGroup}>
            <label style={modalStyles.label}>Tier</label>
            <div style={modalStyles.tierRow}>
              {(["", "short", "long", "permanent"] as const).map((t) => {
                const labels: Record<string, string> = {
                  "": "Auto",
                  short: "Short",
                  long: "Long",
                  permanent: "Permanent",
                };
                const isActive = tier === t;
                return (
                  <button
                    key={t}
                    style={{
                      ...modalStyles.tierBtn,
                      ...(isActive ? modalStyles.tierBtnActive : {}),
                    }}
                    onClick={() => setTier(t as MemoryTier | "")}
                    disabled={saving}
                    type="button"
                    aria-pressed={isActive}
                  >
                    {labels[t]}
                  </button>
                );
              })}
            </div>
          </div>
        </div>

        {/* Footer */}
        <div style={modalStyles.footer}>
          <Button variant="ghost" size="sm" onPress={onClose} isDisabled={saving}>
            Cancel
          </Button>
          <Button
            size="sm"
            variant="secondary"
            onPress={handleSave}
            isDisabled={!content.trim() || saving}
          >
            <Plus size={14} strokeWidth={2} />
            {saving ? "Saving…" : "Save Memory"}
          </Button>
        </div>
      </div>
    </div>
  );
}

const modalStyles: Record<string, React.CSSProperties> = {
  overlay: {
    position: "fixed",
    inset: 0,
    background: "rgba(23, 22, 22, 0.45)",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    zIndex: 1000,
    backdropFilter: "blur(2px)",
  },
  dialog: {
    background: "#fff",
    borderRadius: "var(--radius-card)",
    boxShadow: "0 12px 40px rgba(28,28,28,0.15), 0 2px 8px rgba(28,28,28,0.06)",
    width: "480px",
    maxWidth: "calc(100vw - 32px)",
    maxHeight: "90vh",
    display: "flex",
    flexDirection: "column",
    overflow: "hidden",
  },
  header: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    padding: "16px 20px 14px",
    flexShrink: 0,
  },
  headerLeft: {
    display: "flex",
    alignItems: "center",
    gap: "10px",
  },
  headerIcon: {
    width: "32px",
    height: "32px",
    borderRadius: "8px",
    background: "rgba(147, 51, 234, 0.08)",
    color: "var(--purple-700, #6d28d9)",
    display: "grid",
    placeItems: "center",
    flexShrink: 0,
  },
  title: {
    fontFamily: "var(--font-heading)",
    fontWeight: 700,
    fontSize: "15px",
    margin: 0,
    color: "var(--fg)",
  },
  closeBtn: {
    background: "none",
    border: "none",
    cursor: "pointer",
    color: "var(--grey-500)",
    padding: "6px",
    lineHeight: 1,
    borderRadius: "6px",
    display: "grid",
    placeItems: "center",
    transition: "background 120ms, color 120ms",
  },
  divider: {
    height: "1px",
    background: "var(--grey-100)",
    flexShrink: 0,
  },
  body: {
    flex: 1,
    overflowY: "auto",
    padding: "18px 20px",
    display: "flex",
    flexDirection: "column",
    gap: "16px",
  },
  fieldGroup: {
    display: "flex",
    flexDirection: "column",
    gap: "6px",
  },
  label: {
    fontSize: "11px",
    fontWeight: 600,
    letterSpacing: "0.06em",
    textTransform: "uppercase" as React.CSSProperties["textTransform"],
    color: "var(--grey-500)",
  },
  textarea: {
    width: "100%",
    padding: "10px 12px",
    border: "1px solid var(--grey-200)",
    borderRadius: "8px",
    fontSize: "13.5px",
    fontFamily: "var(--font-body)",
    background: "#fff",
    color: "var(--fg)",
    outline: "none",
    resize: "vertical" as React.CSSProperties["resize"],
    lineHeight: 1.55,
    boxSizing: "border-box" as React.CSSProperties["boxSizing"],
    minHeight: "96px",
    transition: "border-color 120ms",
  },
  selectWrap: {
    position: "relative",
    display: "flex",
    alignItems: "center",
  },
  segDot: {
    position: "absolute",
    left: "10px",
    width: "8px",
    height: "8px",
    borderRadius: "50%",
    flexShrink: 0,
    pointerEvents: "none" as React.CSSProperties["pointerEvents"],
    zIndex: 1,
  },
  select: {
    height: "34px",
    paddingRight: "10px",
    border: "1px solid var(--grey-200)",
    borderRadius: "8px",
    fontSize: "13px",
    fontFamily: "var(--font-body)",
    background: "#fff",
    color: "var(--fg)",
    outline: "none",
    width: "100%",
    boxSizing: "border-box" as React.CSSProperties["boxSizing"],
    cursor: "pointer",
    appearance: "auto" as React.CSSProperties["appearance"],
    transition: "border-color 120ms",
  },
  importanceLabelRow: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
  },
  importanceVal: {
    fontFamily: "var(--font-mono)",
    fontSize: "12px",
    color: "var(--grey-500)",
    minWidth: "32px",
    textAlign: "right" as React.CSSProperties["textAlign"],
  },
  range: {
    width: "100%",
    height: "20px",
    cursor: "pointer",
    boxSizing: "border-box" as React.CSSProperties["boxSizing"],
  },
  rangeLabels: {
    display: "flex",
    justifyContent: "space-between",
    fontSize: "10px",
    color: "var(--grey-400)",
    marginTop: "-2px",
  },
  tierRow: {
    display: "flex",
    gap: "6px",
  },
  tierBtn: {
    flex: 1,
    height: "30px",
    border: "1px solid var(--grey-200)",
    borderRadius: "7px",
    background: "transparent",
    cursor: "pointer",
    fontSize: "12px",
    fontFamily: "var(--font-body)",
    fontWeight: 500,
    color: "var(--grey-600)",
    transition: "background 120ms, border-color 120ms, color 120ms",
  },
  tierBtnActive: {
    background: "rgba(147, 51, 234, 0.07)",
    borderColor: "rgba(147, 51, 234, 0.35)",
    color: "var(--purple-700, #6d28d9)",
    fontWeight: 600,
  },
  footer: {
    display: "flex",
    justifyContent: "flex-end",
    gap: "8px",
    padding: "12px 20px",
    borderTop: "1px solid var(--grey-100)",
    flexShrink: 0,
    background: "var(--grey-50)",
  },
};

// ── Consolidation progress banner ─────────────────────────────

type ConsolidationStatus = "idle" | "running" | "done" | "error";

function ConsolidationBanner({
  status,
  message,
  onStop,
  onDismiss,
}: {
  status: ConsolidationStatus;
  message: string;
  onStop: () => void;
  onDismiss: () => void;
}) {
  if (status === "idle") return null;

  const isRunning = status === "running";
  const isError = status === "error";

  const bannerBg = isError
    ? "rgba(239, 68, 68, 0.06)"
    : isRunning
    ? "rgba(147, 51, 234, 0.05)"
    : "rgba(34, 197, 94, 0.06)";

  const bannerBorder = isError
    ? "rgba(239, 68, 68, 0.2)"
    : isRunning
    ? "rgba(147, 51, 234, 0.2)"
    : "rgba(34, 197, 94, 0.2)";

  const textColor = isError
    ? "var(--color-destructive)"
    : isRunning
    ? "var(--purple-700, #6d28d9)"
    : "#0e8a4a";

  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: 10,
        padding: "10px 14px",
        borderRadius: "10px",
        border: `1px solid ${bannerBorder}`,
        background: bannerBg,
        fontSize: "12.5px",
        color: textColor,
      }}
      role="status"
      aria-live="polite"
    >
      <GitMerge size={14} strokeWidth={1.8} style={{ flexShrink: 0 }} />
      <span style={{ flex: 1 }}>
        {isRunning ? "Consolidating memories…" : ""}{" "}
        {message}
      </span>
      {isRunning && (
        <button
          onClick={onStop}
          style={{
            background: "none",
            border: "1px solid currentColor",
            borderRadius: "6px",
            cursor: "pointer",
            color: "inherit",
            fontSize: "11px",
            padding: "2px 8px",
            fontFamily: "var(--font-body)",
            opacity: 0.8,
          }}
          aria-label="Stop consolidation"
        >
          Stop
        </button>
      )}
      {!isRunning && (
        <button
          onClick={onDismiss}
          style={{
            background: "none",
            border: "none",
            cursor: "pointer",
            color: "inherit",
            padding: "2px",
            display: "grid",
            placeItems: "center",
            opacity: 0.6,
          }}
          aria-label="Dismiss"
        >
          <X size={13} strokeWidth={2} />
        </button>
      )}
    </div>
  );
}

// ── Memory settings toggles ──────────────────────────────────

function MemorySettingsCard({ settings, onToggle }: {
  settings: Partial<Settings>;
  onToggle: (key: string, value: boolean) => void;
}) {
  const toggles: { key: string; label: string; desc: string; value: boolean }[] = [
    {
      key: "agent_memory_inject",
      label: "Inject into prompt",
      desc: "Include recent memories in the LLM system prompt each turn",
      value: settings.agent_memory_inject ?? true,
    },
    {
      key: "memory_extraction_enabled",
      label: "Auto-extraction",
      desc: "Automatically extract facts from conversations in the background",
      value: settings.memory_extraction_enabled ?? true,
    },
    {
      key: "memory_cleanup_enabled",
      label: "Decay and cleanup",
      desc: "Archive low-scoring memories based on time decay (every 6 hours)",
      value: settings.memory_cleanup_enabled ?? true,
    },
    {
      key: "memory_consolidation_enabled",
      label: "Consolidation",
      desc: "Use the LLM to merge similar memories and prune duplicates (every 24 hours)",
      value: settings.memory_consolidation_enabled ?? false,
    },
  ];

  return (
    <Card className="card">
      <CardContent>
        <div style={{ fontSize: "13px", fontWeight: 600, marginBottom: 10, color: "var(--fg)" }}>
          Memory Settings
        </div>
        <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          {toggles.map((t) => (
            <label
              key={t.key}
              style={{
                display: "flex",
                alignItems: "flex-start",
                gap: 10,
                cursor: "pointer",
                padding: "4px 0",
              }}
            >
              <input
                type="checkbox"
                checked={t.value}
                onChange={(e) => onToggle(t.key, e.target.checked)}
                style={{ marginTop: 2, accentColor: "var(--purple-600, #9333ea)" }}
              />
              <div>
                <div style={{ fontSize: "13px", fontWeight: 500, color: "var(--fg)" }}>
                  {t.label}
                </div>
                <div style={{ fontSize: "11px", color: "var(--grey-500)", marginTop: 1 }}>
                  {t.desc}
                </div>
              </div>
            </label>
          ))}
        </div>
      </CardContent>
    </Card>
  );
}

// ── Main Memory component ─────────────────────────────────────

export function Memory() {
  const confirm = useConfirm();
  const [items, setItems]           = useState<MemoryFragment[]>([]);
  const PAGE = 20;
  const [loading, setLoading]       = useState(true);
  const [error, setError]           = useState<string | null>(null);
  const [activeSegment, setSegment] = useState<SegmentKey>("all");
  const [activeTier, setTier]       = useState<TierKey>("all");
  const [activeSource, setSource]   = useState<SourceKey>("all");
  const [searchQuery, setSearchQuery] = useState("");
  const [visibleCount, setVisibleCount] = useState(PAGE);
  const [inlineDraft, setInlineDraft] = useState("");
  const [inlineAdding, setInlineAdding] = useState(false);
  const [showAddModal, setShowAddModal] = useState(false);
  const [memSettings, setMemSettings] = useState<Partial<Settings>>({});

  // Consolidation state
  const [consolidationStatus, setConsolidationStatus] = useState<ConsolidationStatus>("idle");
  const [consolidationMsg, setConsolidationMsg] = useState("");

  function load() {
    setLoading(true);
    setError(null);
    api
      .listMemories(100)
      .then(setItems)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }

  function loadSettings() {
    api.getSettings()
      .then(setMemSettings)
      .catch(() => {}); // non-fatal
  }

  useEffect(() => { load(); loadSettings(); }, []);

  async function handleToggleSetting(key: string, value: boolean) {
    const patch = { [key]: value } as Partial<Settings>;
    setMemSettings((prev) => ({ ...prev, ...patch }));
    try {
      await api.updateSettings(patch);
    } catch (e) {
      setError(String(e));
      loadSettings();
    }
  }

  async function handleAdd(
    content: string,
    segment?: MemorySegment,
    importance?: number,
    tier?: MemoryTier,
  ) {
    try {
      await api.addMemory(content, undefined, segment, importance, tier);
      load();
    } catch (e) {
      setError(String(e));
    }
  }

  // Inline quick-add (no segment/tier selection)
  async function handleInlineAdd() {
    const trimmed = inlineDraft.trim();
    if (!trimmed) return;
    setInlineAdding(true);
    try {
      await api.addMemory(trimmed, undefined, undefined, undefined, undefined);
      setInlineDraft("");
      load();
    } catch (e) {
      setError(String(e));
    } finally {
      setInlineAdding(false);
    }
  }

  async function handleDelete(id: string) {
    if (!await confirm("Delete this memory? This cannot be undone.", { title: "Delete Memory", confirmLabel: "Delete", destructive: true })) return;
    try {
      await api.deleteMemory(id);
      setItems((prev) => prev.filter((m) => m.id !== id));
    } catch (e) {
      setError(String(e));
    }
  }

  // Edit = delete old + create new with same metadata
  async function handleUpdate(id: string, newContent: string) {
    const original = items.find((m) => m.id === id);
    if (!original) return;
    try {
      await api.addMemory(
        newContent,
        original.tags,
        original.segment,
        original.importance,
        original.tier,
      );
      await api.deleteMemory(id);
      setItems((prev) =>
        prev.map((m) =>
          m.id === id
            ? { ...m, content: newContent }
            : m,
        ),
      );
      load();
    } catch (e) {
      setError(String(e));
      throw e;
    }
  }

  // Only show active lifecycle memories — with fallback for old API without lifecycle
  const visibleItems = items.filter(
    (m) => !m.lifecycle || m.lifecycle === "active",
  );

  async function handleDeleteAll() {
    const count = visibleItems.length;
    if (!count) return;
    if (!await confirm(`Delete all ${count} memories? This cannot be undone.`, { title: "Delete All Memories", confirmLabel: "Delete All", destructive: true })) return;
    const errs: string[] = [];
    for (const m of visibleItems) {
      try { await api.deleteMemory(m.id); }
      catch (e) { errs.push(String(e)); }
    }
    load();
    if (errs.length) setError(`${errs.length} deletion(s) failed.`);
  }

  // Consolidation
  async function handleConsolidate() {
    if (consolidationStatus === "running") return;
    setConsolidationStatus("running");
    setConsolidationMsg("");
    try {
      for await (const event of api.streamConsolidation()) {
        switch (event.type) {
          case "started":
            setConsolidationMsg(`Analyzing ${event.memory_count ?? ""} memories...`);
            break;
          case "proposer_done":
            setConsolidationMsg(`Proposer found ${(event.proposals as unknown[])?.length ?? 0} changes`);
            break;
          case "adversary_done":
            setConsolidationMsg("Adversary reviewing proposals...");
            break;
          case "judge_done":
            setConsolidationMsg("Judge making final decisions...");
            break;
          case "applied":
            setConsolidationMsg("Applying accepted changes...");
            break;
          case "completed":
            if (event.result) {
              const { accepted_count, rejected_count, duration_ms } = event.result;
              const secs = (duration_ms / 1000).toFixed(1);
              setConsolidationMsg(
                `Done in ${secs}s — ${accepted_count} accepted, ${rejected_count} rejected.`,
              );
            } else {
              setConsolidationMsg("Done.");
            }
            setConsolidationStatus("done");
            load();
            break;
          case "error":
            setConsolidationMsg(event.message ?? "Consolidation failed.");
            setConsolidationStatus("error");
            break;
          case "cancelled":
            setConsolidationMsg("Cancelled.");
            setConsolidationStatus("done");
            break;
        }
        if (event.type === "completed" || event.type === "error" || event.type === "cancelled") break;
      }
    } catch (e) {
      setConsolidationMsg(String(e));
      setConsolidationStatus("error");
    }
  }

  async function handleStopConsolidation() {
    try {
      await api.stopConsolidation();
    } catch {
      // best-effort
    }
    setConsolidationStatus("done");
    setConsolidationMsg("Cancelled.");
  }

  // Segment counts for filter row
  const segCounts = SEGMENT_ORDER.reduce<Partial<Record<MemorySegment, number>>>((acc, seg) => {
    const n = visibleItems.filter((m) => m.segment === seg).length;
    if (n > 0) acc[seg] = n;
    return acc;
  }, {});

  // Tier counts
  const tierCounts = (["short", "long", "permanent"] as MemoryTier[]).reduce<Partial<Record<MemoryTier, number>>>((acc, tier) => {
    const n = visibleItems.filter((m) => m.tier === tier).length;
    if (n > 0) acc[tier] = n;
    return acc;
  }, {});

  // Source counts
  const sourceCounts = (["auto", "mcp", "chat"] as SourceKey[]).reduce<Partial<Record<SourceKey, number>>>((acc, src) => {
    const n = visibleItems.filter((m) => normalizedSource(m.source) === src).length;
    if (n > 0) acc[src] = n;
    return acc;
  }, {});

  // Apply all filters
  const filtered = visibleItems.filter((m) => {
    if (activeSegment !== "all" && m.segment !== activeSegment) return false;
    if (activeTier !== "all" && m.tier !== activeTier) return false;
    if (activeSource !== "all" && normalizedSource(m.source) !== activeSource) return false;
    if (searchQuery) {
      const q = searchQuery.toLowerCase();
      if (!m.content.toLowerCase().includes(q)) return false;
    }
    return true;
  });

  const hasActiveFilters = activeSegment !== "all" || activeTier !== "all" || activeSource !== "all" || searchQuery !== "";

  return (
    <div className="screen">
      {/* Page header */}
      <PageHeader
        title="Memory"
        action={
          <>
            <Chip size="sm" variant="soft">
              {loading ? "…" : visibleItems.length} memories
            </Chip>
            <Button size="sm" variant="ghost" onPress={load} isDisabled={loading}>
              <RefreshCw size={14} strokeWidth={1.8} style={{ opacity: loading ? 0.4 : 1 }} />
              Refresh
            </Button>
            <Button
              size="sm"
              variant="ghost"
              onPress={handleConsolidate}
              isDisabled={consolidationStatus === "running" || loading || visibleItems.length === 0}
            >
              <GitMerge size={14} strokeWidth={1.8} />
              Consolidate
            </Button>
            {!loading && visibleItems.length > 0 && (
              <Button size="sm" variant="danger-soft" onPress={handleDeleteAll}>
                <Trash2 size={14} strokeWidth={1.8} />
                Delete All
              </Button>
            )}
          </>
        }
      />

      {/* Add memory modal */}
      {showAddModal && (
        <AddMemoryModal
          onAdd={handleAdd}
          onClose={() => setShowAddModal(false)}
        />
      )}

      {/* Consolidation progress */}
      <ConsolidationBanner
        status={consolidationStatus}
        message={consolidationMsg}
        onStop={handleStopConsolidation}
        onDismiss={() => { setConsolidationStatus("idle"); setConsolidationMsg(""); }}
      />

      {/* Error */}
      {error && (
        <p style={{ color: "var(--color-destructive)", fontSize: "var(--text-sm)", margin: 0 }}>
          {error}
        </p>
      )}

      {/* Inline add form — design-spec card */}
      <Card className="card">
        <CardContent>
          <div className="mem-add">
            <div className="mem-add__input" style={{ position: "relative", flex: 1, display: "flex", alignItems: "center" }}>
              <span style={{ position: "absolute", left: 10, pointerEvents: "none", color: "var(--grey-400)", display: "flex", zIndex: 1 }}>
                <Sparkles size={14} strokeWidth={1.8} />
              </span>
              <input
                type="text"
                placeholder="Add a memory…"
                value={inlineDraft}
                onChange={(e) => setInlineDraft(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Enter") handleInlineAdd(); }}
                disabled={inlineAdding}
                style={{
                  width: "100%",
                  height: "36px",
                  padding: "0 12px 0 34px",
                  border: "1px solid var(--grey-200)",
                  borderRadius: "var(--radius-md)",
                  fontSize: "13px",
                  fontFamily: "var(--font-body)",
                  background: "#fff",
                  color: "var(--fg)",
                  outline: "none",
                  boxSizing: "border-box" as React.CSSProperties["boxSizing"],
                  transition: "border-color 120ms",
                }}
                aria-label="Quick add memory"
              />
            </div>
            <Button
              size="sm"
              variant="secondary"
              onPress={handleInlineAdd}
              isDisabled={!inlineDraft.trim() || inlineAdding}
            >
              <Plus size={14} strokeWidth={2} />
              {inlineAdding ? "Adding…" : "Add"}
            </Button>
            <Button
              size="sm"
              variant="ghost"
              onPress={() => setShowAddModal(true)}
            >
              Advanced
            </Button>
          </div>
        </CardContent>
      </Card>

      {/* Stats row */}
      {!loading && visibleItems.length > 0 && (
        <MemStatsBar items={visibleItems} />
      )}

      {/* Search bar */}
      {!loading && visibleItems.length > 0 && (
        <div className="mem-search">
          <span className="mem-search__icon">
            <Search size={14} strokeWidth={1.8} />
          </span>
          <input
            className="mem-search__input"
            type="search"
            placeholder="Search memories…"
            value={searchQuery}
            onChange={(e) => { setSearchQuery(e.target.value); setVisibleCount(PAGE); }}
            aria-label="Search memories"
          />
          {searchQuery && (
            <button
              className="mem-search__clear"
              onClick={() => setSearchQuery("")}
              aria-label="Clear search"
              type="button"
            >
              <X size={11} strokeWidth={2.5} />
            </button>
          )}
        </div>
      )}

      {/* Filter row */}
      {!loading && visibleItems.length > 0 && (
        <MemFilterRow
          activeSegment={activeSegment}
          activeTier={activeTier}
          activeSource={activeSource}
          segCounts={segCounts}
          tierCounts={tierCounts}
          sourceCounts={sourceCounts}
          total={visibleItems.length}
          onSegmentChange={(v) => { setSegment(v); setVisibleCount(PAGE); }}
          onTierChange={(v) => { setTier(v); setVisibleCount(PAGE); }}
          onSourceChange={(v) => { setSource(v); setVisibleCount(PAGE); }}
        />
      )}

      {/* Memory list card */}
      {loading ? (
        <SkeletonList rows={5} />
      ) : visibleItems.length === 0 ? (
        <Card className="card">
          <CardContent>
            <div className="empty-state">
              <Brain size={36} strokeWidth={1.2} />
              <div style={{ maxWidth: 320 }}>
                <div style={{ fontWeight: "var(--weight-semibold)", marginBottom: 6, fontSize: "15px" }}>
                  No memories yet
                </div>
                <div style={{ fontSize: "var(--text-sm)", color: "var(--grey-500)", lineHeight: 1.6 }}>
                  The assistant builds up memory as you chat — facts about you, your preferences, and ongoing projects get saved automatically.
                </div>
                <div style={{ marginTop: 12, display: "flex", flexDirection: "column", gap: 6 }}>
                  <div style={{ display: "flex", alignItems: "center", gap: 8, fontSize: "12px", color: "var(--grey-500)" }}>
                    <MessageSquare size={13} strokeWidth={1.8} style={{ flexShrink: 0, color: "var(--grey-400)" }} />
                    Chat with the assistant to create memories automatically
                  </div>
                  <div style={{ display: "flex", alignItems: "center", gap: 8, fontSize: "12px", color: "var(--grey-500)" }}>
                    <Plus size={13} strokeWidth={2} style={{ flexShrink: 0, color: "var(--grey-400)" }} />
                    Or add one manually with the form above
                  </div>
                </div>
              </div>
            </div>
          </CardContent>
        </Card>
      ) : filtered.length === 0 ? (
        <div className="empty-state empty-state--inline">
          <Search size={18} strokeWidth={1.8} />
          <span>
            {searchQuery
              ? `No memories match "${searchQuery}".`
              : hasActiveFilters
              ? "No memories match the current filters."
              : "No memories."}
          </span>
        </div>
      ) : (
        <>
          <Card className="card">
            <CardContent className="card-body--list">
              {filtered.slice(0, visibleCount).map((m) => (
                <MemRow
                  key={m.id}
                  mem={m}
                  onDelete={handleDelete}
                  onUpdate={handleUpdate}
                />
              ))}
            </CardContent>
          </Card>
          {filtered.length > visibleCount && (
            <button className="empty-state__cta" style={{ alignSelf: "center" }} onClick={() => setVisibleCount((c) => c + PAGE)}>
              Show {Math.min(PAGE, filtered.length - visibleCount)} more
              <span style={{ color: "var(--grey-400)", fontWeight: "normal" }}> · {filtered.length - visibleCount} remaining</span>
            </button>
          )}
        </>
      )}

      {/* Memory lifecycle settings */}
      <MemorySettingsCard settings={memSettings} onToggle={handleToggleSetting} />
    </div>
  );
}
