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
} from "lucide-react";
import { api } from "../api/PondApiClient";
import type { MemoryFragment, MemorySegment, MemoryTier } from "../api/types";

// ── Segment metadata ──────────────────────────────────────────

type SegmentKey = MemorySegment | "all";

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
  active,
  counts,
  total,
  onChange,
}: {
  active: SegmentKey;
  counts: Partial<Record<MemorySegment, number>>;
  total: number;
  onChange: (seg: SegmentKey) => void;
}) {
  return (
    <div className="mem-filter">
      <button
        className={`mem-filter__btn${active === "all" ? " is-active" : ""}`}
        onClick={() => onChange("all")}
      >
        All
        <span className="mem-filter__count">{total}</span>
      </button>
      {SEGMENT_ORDER.filter((s) => counts[s]).map((seg) => (
        <button
          key={seg}
          className={`mem-filter__btn${active === seg ? " is-active" : ""}`}
          onClick={() => onChange(seg)}
        >
          {SEGMENTS[seg].label}
          <span className="mem-filter__count">{counts[seg]}</span>
        </button>
      ))}
    </div>
  );
}

// ── Memory card ───────────────────────────────────────────────

function MemCard({
  mem,
  onDelete,
}: {
  mem: MemoryFragment;
  onDelete: (id: string) => void;
}) {
  const seg = mem.segment;
  const segMeta = seg ? SEGMENTS[seg] : null;
  const segClass = seg ? seg : "none";
  const importanceFill = seg ? SEGMENT_IMPORT_FILL[seg] : "var(--grey-300)";
  const importance = mem.importance ?? (seg ? segMeta?.importanceDefault : undefined);

  return (
    <div className="mem-card">
      <div className="mem-card__header">
        {/* Segment icon */}
        <div className={`mem-card__seg-icon mem-card__seg-icon--${segClass}`}>
          {segMeta ? segMeta.icon : <BrainCircuit size={13} strokeWidth={1.8} />}
        </div>

        <div className="mem-card__body">
          {/* Top badge row */}
          <div className="mem-card__top">
            {seg ? (
              <span className={`mem-seg-badge mem-seg-badge--${segClass}`}>
                {segMeta?.icon}
                {segMeta?.label ?? seg}
              </span>
            ) : (
              <span className="mem-seg-badge mem-seg-badge--none">
                <BrainCircuit size={10} strokeWidth={1.8} />
                Memory
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
          </div>

          {/* Content */}
          <div className="mem-card__content">{mem.content}</div>

          {/* Importance bar */}
          {importance !== undefined && (
            <div className="mem-importance">
              <div className="mem-importance__track">
                <div
                  className="mem-importance__fill"
                  style={{
                    width: `${Math.round(importance * 100)}%`,
                    background: importanceFill,
                  }}
                />
              </div>
              <span className="mem-importance__label">{Math.round(importance * 10) / 10}</span>
            </div>
          )}

          {/* Footer meta */}
          <div className="mem-card__meta">
            <span className="mem-card__meta-item">
              <Clock size={10} strokeWidth={1.8} />
              {relativeTime(mem.created_at)}
            </span>
            {mem.access_count > 0 && (
              <span className="mem-card__meta-item">
                <Star size={10} strokeWidth={1.8} />
                accessed {mem.access_count}x
              </span>
            )}
            {mem.last_accessed_at && (
              <span className="mem-card__meta-item">
                last {relativeTime(mem.last_accessed_at)}
              </span>
            )}
            {mem.tags && mem.tags.length > 0 && (
              <span className="mem-card__meta-item">
                {mem.tags.slice(0, 3).join(", ")}
              </span>
            )}
          </div>
        </div>

        {/* Delete button */}
        <button
          className="mem-card__delete"
          onClick={() => onDelete(mem.id)}
          aria-label="Delete memory"
          title="Delete memory"
        >
          <Trash2 size={13} strokeWidth={1.8} />
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

  // Auto-focus textarea on open
  useEffect(() => {
    textareaRef.current?.focus();
  }, []);

  // Close on Escape
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
            color="secondary"
            onPress={handleSave}
            isDisabled={!content.trim() || saving}
            isLoading={saving}
          >
            {!saving && <Plus size={14} strokeWidth={2} />}
            Save Memory
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

// ── Main Memory component ─────────────────────────────────────

export function Memory() {
  const [items, setItems]         = useState<MemoryFragment[]>([]);
  const [loading, setLoading]     = useState(true);
  const [error, setError]         = useState<string | null>(null);
  const [activeFilter, setFilter] = useState<SegmentKey>("all");
  const [showAddModal, setShowAddModal] = useState(false);

  function load() {
    setLoading(true);
    setError(null);
    api
      .listMemories(100)
      .then(setItems)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }

  useEffect(() => { load(); }, []);

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

  async function handleDelete(id: string) {
    try {
      await api.deleteMemory(id);
      setItems((prev) => prev.filter((m) => m.id !== id));
    } catch (e) {
      setError(String(e));
    }
  }

  // Only show active lifecycle memories (skip archived/merged) — with fallback for old API without lifecycle
  const visibleItems = items.filter(
    (m) => !m.lifecycle || m.lifecycle === "active",
  );

  // Segment counts for filter row
  const segCounts = SEGMENT_ORDER.reduce<Partial<Record<MemorySegment, number>>>((acc, seg) => {
    const n = visibleItems.filter((m) => m.segment === seg).length;
    if (n > 0) acc[seg] = n;
    return acc;
  }, {});

  // Filtered list
  const filtered =
    activeFilter === "all"
      ? visibleItems
      : visibleItems.filter((m) => m.segment === activeFilter);

  return (
    <div className="screen">
      {/* Page header */}
      <div className="page-header">
        <h1 className="page-header__title">Memory</h1>
        <div className="page-header__action">
          <Chip size="sm" variant="flat" color="secondary">
            {loading ? "..." : visibleItems.length}
          </Chip>
          <Button size="sm" variant="ghost" onPress={load} isDisabled={loading}>
            <RefreshCw size={14} strokeWidth={1.8} style={{ opacity: loading ? 0.4 : 1 }} />
            Refresh
          </Button>
          <Button size="sm" color="secondary" onPress={() => setShowAddModal(true)}>
            <Plus size={14} strokeWidth={2} />
            Add Memory
          </Button>
        </div>
      </div>

      {/* Add memory modal */}
      {showAddModal && (
        <AddMemoryModal
          onAdd={handleAdd}
          onClose={() => setShowAddModal(false)}
        />
      )}

      {/* Error */}
      {error && (
        <p style={{ color: "var(--color-destructive)", fontSize: "var(--text-sm)", margin: 0 }}>
          {error}
        </p>
      )}

      {/* Stats + filter row */}
      {!loading && visibleItems.length > 0 && (
        <>
          <MemStatsBar items={visibleItems} />
          <MemFilterRow
            active={activeFilter}
            counts={segCounts}
            total={visibleItems.length}
            onChange={(seg) => setFilter(seg)}
          />
        </>
      )}

      {/* Memory list */}
      {loading ? (
        <p className="muted-12">Loading...</p>
      ) : visibleItems.length === 0 ? (
        <div className="empty-state">
          <BrainCircuit size={32} strokeWidth={1.2} />
          <div>
            <div style={{ fontWeight: "var(--weight-semibold)", marginBottom: 4 }}>
              No memories yet
            </div>
            <div style={{ fontSize: "var(--text-sm)", color: "var(--grey-500)" }}>
              Memories are auto-extracted from conversations, or add one manually above.
            </div>
          </div>
        </div>
      ) : filtered.length === 0 ? (
        <div className="empty-state empty-state--inline">
          <BrainCircuit size={18} />
          <span>No {activeFilter !== "all" ? SEGMENTS[activeFilter as MemorySegment]?.label.toLowerCase() : ""} memories.</span>
        </div>
      ) : (
        <Card shadow="none" className="giap-card">
          <CardContent>
            <div className="mem-list">
              {filtered.map((m) => (
                <MemCard key={m.id} mem={m} onDelete={handleDelete} />
              ))}
            </div>
          </CardContent>
        </Card>
      )}
    </div>
  );
}
