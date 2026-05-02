import { useState, useEffect } from "react";
import { Card, CardContent, Button, Chip, Input } from "@heroui/react";
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
  ChevronDown,
  ChevronUp,
  RefreshCw,
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

// ── Add memory form ───────────────────────────────────────────

function AddMemoryForm({
  onAdd,
  disabled,
}: {
  onAdd: (
    content: string,
    segment?: MemorySegment,
    importance?: number,
    tier?: MemoryTier,
  ) => Promise<void>;
  disabled: boolean;
}) {
  const [input, setInput] = useState("");
  const [saving, setSaving] = useState(false);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [segment, setSegment] = useState<MemorySegment | "">("");
  const [importance, setImportance] = useState(0.7);
  const [tier, setTier] = useState<MemoryTier | "">("");

  async function handleAdd() {
    const content = input.trim();
    if (!content) return;
    setSaving(true);
    try {
      await onAdd(
        content,
        segment !== "" ? (segment as MemorySegment) : undefined,
        showAdvanced && segment !== "" ? importance : undefined,
        tier !== "" ? (tier as MemoryTier) : undefined,
      );
      setInput("");
      if (!showAdvanced) {
        // Keep advanced settings across adds when panel is open
      }
    } finally {
      setSaving(false);
    }
  }

  return (
    <Card shadow="none" className="giap-card">
      <CardContent>
        <div className="mem-add-form">
          <div className="mem-add-form__row">
            <Input
              className="mem-add__input"
              size="sm"
              radius="md"
              variant="bordered"
              placeholder="Add a memory..."
              aria-label="New memory content"
              value={input}
              onValueChange={setInput}
              onKeyDown={(e: React.KeyboardEvent) => e.key === "Enter" && handleAdd()}
              startContent={<BrainCircuit size={14} />}
            />
            <Button
              size="sm"
              color="secondary"
              onPress={handleAdd}
              isDisabled={!input.trim() || saving || disabled}
            >
              <Plus size={14} strokeWidth={2} />
              Add
            </Button>
          </div>

          <button
            className="mem-add-form__toggle"
            onClick={() => setShowAdvanced((v) => !v)}
            type="button"
          >
            {showAdvanced ? <ChevronUp size={12} /> : <ChevronDown size={12} />}
            {showAdvanced ? "Hide options" : "Set segment, importance, tier"}
          </button>

          {showAdvanced && (
            <div className="mem-add-form__fields">
              {/* Segment */}
              <div>
                <div className="mem-add-form__field-label">Segment</div>
                <select
                  className="mem-add-form__select"
                  value={segment}
                  onChange={(e) => setSegment(e.target.value as MemorySegment | "")}
                  disabled={saving}
                >
                  <option value="">Auto-classify</option>
                  {SEGMENT_ORDER.map((s) => (
                    <option key={s} value={s}>{SEGMENTS[s].label}</option>
                  ))}
                </select>
              </div>

              {/* Importance */}
              <div>
                <div className="mem-add-form__field-label">Importance</div>
                <div className="mem-add-form__importance-row">
                  <input
                    type="range"
                    className="mem-add-form__range"
                    min={0}
                    max={1}
                    step={0.05}
                    value={importance}
                    onChange={(e) => setImportance(parseFloat(e.target.value))}
                    disabled={saving || segment === ""}
                    title={segment === "" ? "Select a segment first" : `Importance: ${importance}`}
                  />
                  <span className="mem-add-form__importance-val">
                    {segment !== "" ? importance.toFixed(2) : "–"}
                  </span>
                </div>
              </div>

              {/* Tier */}
              <div>
                <div className="mem-add-form__field-label">Tier</div>
                <select
                  className="mem-add-form__select"
                  value={tier}
                  onChange={(e) => setTier(e.target.value as MemoryTier | "")}
                  disabled={saving}
                >
                  <option value="">Auto (by segment)</option>
                  <option value="short">Short</option>
                  <option value="long">Long</option>
                  <option value="permanent">Permanent</option>
                </select>
              </div>
            </div>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

// ── Main Memory component ─────────────────────────────────────

export function Memory() {
  const [items, setItems]         = useState<MemoryFragment[]>([]);
  const [loading, setLoading]     = useState(true);
  const [error, setError]         = useState<string | null>(null);
  const [activeFilter, setFilter] = useState<SegmentKey>("all");

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
        </div>
      </div>

      {/* Add form */}
      <AddMemoryForm onAdd={handleAdd} disabled={false} />

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
