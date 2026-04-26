import { useState, useEffect, useRef, useCallback } from "react";
import { Button } from "@heroui/react";
import {
  Brain, Mic, Volume2, RefreshCw, Download, CheckCircle,
  ChevronDown, ChevronUp, Search, Trash2, MessageSquare, Wrench, Play,
  ScanFace,
} from "lucide-react";
import { api } from "../api/PondApiClient";
import { useAppState } from "../state/AppContext";
import type {
  ModelEntry, ModelActiveRoles, ModelMemoryStatus, HfModel, HfModelFile, DownloadEntry,
  OllamaModel, LlamafileRelease, FaceModelsResponse,
} from "../api/types";
import { ApiError } from "../api/types";

// ── Design constants ──────────────────────────────────────────

const CAT_COLOR = {
  llm:  "var(--color-accent)",
  asr:  "#e5a000",
  tts:  "#e04343",
  face: "#3b82f6",
} as const;

const ROLE_COLOR: Record<string, string> = {
  chat:  "var(--color-accent)",
  think: "#8c52ff",
  task:  "#30a46c",
  asr:   "#e5a000",
  tts:   "#e04343",
};

// ── Active Roles Banner ───────────────────────────────────────

function ActiveRolesBanner({
  roles,
  onRefresh,
  loading,
  onNavigate,
}: {
  roles: ModelActiveRoles | null;
  onRefresh: () => void;
  loading: boolean;
  onNavigate?: (category: "llm" | "asr" | "tts") => void;
}) {
  const ROLE_DEFS = [
    { key: "chat"  as const, label: "Chat",  color: ROLE_COLOR.chat,  icon: <MessageSquare size={10} />, category: "llm" as const },
    { key: "think" as const, label: "Think", color: ROLE_COLOR.think, icon: <Brain size={10} />,         category: "llm" as const },
    { key: "task"  as const, label: "Task",  color: ROLE_COLOR.task,  icon: <Wrench size={10} />,        category: "llm" as const },
    { key: "asr"   as const, label: "ASR",   color: ROLE_COLOR.asr,   icon: <Mic size={10} />,           category: "asr" as const },
    { key: "tts"   as const, label: "TTS",   color: ROLE_COLOR.tts,   icon: <Volume2 size={10} />,       category: "tts" as const },
  ];

  return (
    <div style={bannerSt.root}>
      <div style={bannerSt.header}>
        <span style={bannerSt.title}>Active Model Roles</span>
        <button style={bannerSt.refreshBtn} onClick={onRefresh} disabled={loading} aria-label="Refresh roles">
          <RefreshCw size={13} style={{ opacity: loading ? 0.4 : 1, transition: "opacity 0.2s" }} />
        </button>
      </div>
      <div style={bannerSt.pills}>
        {ROLE_DEFS.map(({ key, label, color, icon, category }) => {
          const a = roles?.[key];
          const isSet = !!(a?.provider && a?.model);
          return (
            <div
              key={key}
              style={{ ...bannerSt.pill, borderLeftColor: color, cursor: !isSet && onNavigate ? "pointer" : "default" }}
              onClick={!isSet && onNavigate ? () => onNavigate(category) : undefined}
              title={!isSet ? `Click to set ${label} model` : undefined}
            >
              <div style={{ ...bannerSt.roleLabel, color }}>
                {icon}
                <span>{label}</span>
              </div>
              <span style={isSet ? bannerSt.modelName : bannerSt.notSet}>
                {isSet ? `${a!.provider} / ${a!.model}` : "Not set"}
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

const bannerSt: Record<string, React.CSSProperties> = {
  root: {
    background: "var(--color-bg)",
    border: "1px solid var(--color-border)",
    borderRadius: "var(--radius-lg)",
    padding: "var(--space-4)",
    display: "flex",
    flexDirection: "column",
    gap: "var(--space-3)",
  },
  header: { display: "flex", alignItems: "center", justifyContent: "space-between" },
  title: {
    fontFamily: "var(--font-display)",
    fontWeight: 700,
    fontSize: "var(--text-sm)",
    color: "var(--color-text)",
    textTransform: "uppercase" as const,
    letterSpacing: "0.06em",
  },
  refreshBtn: {
    background: "none", border: "none", cursor: "pointer",
    padding: "var(--space-1)", color: "var(--color-text-tertiary)",
    display: "flex", alignItems: "center",
  },
  pills: { display: "flex", flexWrap: "wrap" as const, gap: "var(--space-2)" },
  pill: {
    display: "flex",
    flexDirection: "column" as const,
    gap: "3px",
    background: "rgba(23,22,22,0.02)",
    border: "1px solid var(--color-border)",
    borderLeft: "4px solid",
    padding: "var(--space-2) var(--space-3)",
    minWidth: "130px",
    transition: "border-color var(--transition-fast)",
  },
  roleLabel: {
    display: "flex", alignItems: "center", gap: "4px",
    fontSize: "9px", fontWeight: 700,
    textTransform: "uppercase" as const, letterSpacing: "0.08em",
    fontFamily: "var(--font-display)",
  },
  modelName: {
    fontSize: "var(--text-xs)", fontFamily: "var(--font-mono)",
    color: "var(--color-text)", fontWeight: 500,
    whiteSpace: "nowrap" as const, overflow: "hidden", textOverflow: "ellipsis", maxWidth: "200px",
  },
  notSet: {
    fontSize: "var(--text-xs)", fontFamily: "var(--font-mono)",
    color: "var(--color-text-tertiary)", fontStyle: "italic",
  },
};

// ── Download Progress ─────────────────────────────────────────

function DownloadProgress({ downloads, onScanModels }: { downloads: DownloadEntry[]; onScanModels: () => void }) {
  if (downloads.length === 0) return null;
  const allDone = downloads.every((d) => d.status === "done" || d.status === "error");
  return (
    <div style={dlSt.root}>
      <div style={dlSt.header}>
        <span style={dlSt.title}>Downloads</span>
        {allDone && (
          <Button variant="outline" size="sm" onPress={onScanModels}>
            <RefreshCw size={12} /> Scan & index
          </Button>
        )}
      </div>
      {downloads.map((d) => (
        <div key={d.filename} style={dlSt.item}>
          <div style={dlSt.itemHeader}>
            <code style={dlSt.filename}>{d.filename}</code>
            <span style={{
              ...dlSt.status,
              color: d.status === "error" ? "var(--color-destructive)" : d.status === "done" ? "var(--color-success)" : "var(--color-text-secondary)",
            }}>
              {d.status === "done" ? "Complete" : d.status === "error" ? (d.error ?? "Error") : `${d.progress_pct ?? 0}%`}
            </span>
          </div>
          <div style={dlSt.track}>
            <div style={{
              ...dlSt.fill,
              width: `${d.progress_pct ?? 0}%`,
              background: d.status === "error" ? "var(--color-destructive)" : d.status === "done" ? "var(--color-success)" : "var(--color-accent)",
            }} />
          </div>
        </div>
      ))}
    </div>
  );
}

const dlSt: Record<string, React.CSSProperties> = {
  root: {
    display: "flex", flexDirection: "column", gap: "var(--space-2)",
    background: "var(--color-bg)", border: "1px solid var(--color-border)",
    borderRadius: "var(--radius-md)", padding: "var(--space-3) var(--space-4)",
  },
  header: { display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: "2px" },
  title: { fontSize: "var(--text-xs)", fontWeight: 700, color: "var(--color-text-secondary)", textTransform: "uppercase" as const, letterSpacing: "0.06em" },
  item: { display: "flex", flexDirection: "column" as const, gap: "4px" },
  itemHeader: { display: "flex", alignItems: "center", justifyContent: "space-between", gap: "var(--space-2)" },
  filename: { fontFamily: "var(--font-mono)", fontSize: "var(--text-xs)", color: "var(--color-text)", flex: 1, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" as const },
  status: { fontSize: "var(--text-xs)", flexShrink: 0, fontFamily: "var(--font-mono)" },
  track: { height: "4px", background: "rgba(23,22,22,0.08)", borderRadius: "2px", overflow: "hidden" },
  fill: { height: "100%", borderRadius: "2px", transition: "width 0.4s ease" },
};

// ── Shared ModelList ──────────────────────────────────────────

type RoleKey = "chat" | "think" | "task" | "asr" | "tts";

const ROLE_LABELS: Record<RoleKey, string> = {
  chat: "Chat", think: "Think", task: "Task", asr: "ASR", tts: "TTS",
};

function ModelList({
  models,
  loading,
  error,
  activeRoles,
  availableRoles,
  onActivate,
  onDelete,
  emptyMessage,
}: {
  models: ModelEntry[];
  loading: boolean;
  error: string | null;
  activeRoles: ModelActiveRoles | null;
  availableRoles: RoleKey[];
  onActivate: (provider: string, name: string, role: string) => void;
  onDelete: (provider: string, name: string) => void;
  emptyMessage: string;
}) {
  const state = useAppState();

  if (loading) return <p style={hint}>Loading…</p>;
  if (error) return <p style={{ ...hint, color: "var(--color-destructive)" }}>{error}</p>;
  if (models.length === 0) return <p style={hint}>{emptyMessage}</p>;

  function isRoleActive(m: ModelEntry, role: RoleKey) {
    const a = activeRoles?.[role];
    if (!a) return false;
    return a.provider === m.provider && a.model === m.name;
  }

  function activeRolesFor(m: ModelEntry): RoleKey[] {
    return availableRoles.filter((r) => isRoleActive(m, r));
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "var(--space-2)" }}>
      {models.map((m) => {
        const activeFor = activeRolesFor(m);
        const isAnyActive = activeFor.length > 0;
        const accentColor = activeFor.length > 0 ? ROLE_COLOR[activeFor[0]] : "transparent";

        return (
          <div
            key={m.id}
            style={{
              ...mlSt.row,
              borderLeftColor: isAnyActive ? accentColor : "transparent",
            }}
          >
            <div style={mlSt.left}>
              <div style={mlSt.nameRow}>
                <span style={mlSt.name}>{m.display_name ?? m.name}</span>
                {m.ram_estimate_mb && (
                  <span style={mlSt.badge}>{m.ram_estimate_mb} MB</span>
                )}
                {m.recommended_role && (
                  <span style={mlSt.badge}>{m.recommended_role}</span>
                )}
              </div>
              <span style={mlSt.sub}>{m.provider} / {m.name}</span>
            </div>
            <div style={mlSt.actions}>
              {availableRoles.map((role) => {
                const active = isRoleActive(m, role);
                return (
                  <button
                    key={role}
                    style={{
                      ...mlSt.roleBtn,
                      background: active ? ROLE_COLOR[role] : "transparent",
                      color: active ? "#fff" : "var(--color-text-secondary)",
                      borderColor: active ? ROLE_COLOR[role] : "var(--color-border-strong)",
                    }}
                    onClick={() => onActivate(m.provider, m.name, role)}
                    disabled={!state.serverOnline}
                    aria-label={`Set ${m.name} as ${role} model`}
                    title={`Set as ${ROLE_LABELS[role]}`}
                  >
                    {active && <CheckCircle size={11} />}
                    {ROLE_LABELS[role]}
                  </button>
                );
              })}
              <button
                style={mlSt.deleteBtn}
                onClick={() => onDelete(m.provider, m.name)}
                disabled={!state.serverOnline}
                aria-label={`Delete ${m.name}`}
                title="Delete from disk"
              >
                <Trash2 size={12} />
              </button>
            </div>
          </div>
        );
      })}
    </div>
  );
}

const mlSt: Record<string, React.CSSProperties> = {
  row: {
    display: "flex", alignItems: "center", gap: "var(--space-3)",
    padding: "var(--space-3) var(--space-4)",
    background: "var(--color-bg)",
    border: "1px solid var(--color-border)",
    borderLeft: "4px solid",
    transition: "border-color var(--transition-fast), box-shadow var(--transition-fast)",
  },
  left: { flex: 1, display: "flex", flexDirection: "column" as const, gap: "2px", minWidth: 0 },
  nameRow: { display: "flex", alignItems: "center", gap: "var(--space-2)", flexWrap: "wrap" as const },
  name: {
    fontWeight: 600, fontSize: "var(--text-sm)", color: "var(--color-text)",
    overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" as const,
  },
  sub: {
    fontSize: "var(--text-xs)", fontFamily: "var(--font-mono)",
    color: "var(--color-text-tertiary)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" as const,
  },
  badge: {
    fontSize: "var(--text-xs)", fontFamily: "var(--font-mono)",
    color: "var(--color-text-tertiary)", background: "rgba(23,22,22,0.05)",
    padding: "1px 6px", borderRadius: "var(--radius-xs)", flexShrink: 0,
  },
  actions: { display: "flex", alignItems: "center", gap: "var(--space-1)", flexShrink: 0 },
  roleBtn: {
    display: "inline-flex", alignItems: "center", gap: "4px",
    height: "28px", padding: "0 10px",
    border: "1px solid", borderRadius: "var(--radius-md)",
    fontSize: "var(--text-xs)", fontWeight: 600, fontFamily: "var(--font-body)",
    cursor: "pointer", transition: "all var(--transition-fast)",
    whiteSpace: "nowrap" as const,
  },
  deleteBtn: {
    display: "inline-flex", alignItems: "center", justifyContent: "center",
    height: "28px", width: "28px",
    border: "1px solid var(--color-border-strong)", borderRadius: "var(--radius-md)",
    background: "transparent", color: "var(--color-destructive)",
    cursor: "pointer", transition: "all var(--transition-fast)", flexShrink: 0,
  },
};

// ── Browse HuggingFace Accordion ──────────────────────────────

function BrowseHfAccordion({ onDownloadStarted }: { onDownloadStarted: () => void }) {
  const state = useAppState();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("gemma");
  const [searching, setSearching] = useState(false);
  const [results, setResults] = useState<HfModel[]>([]);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [expandedRepo, setExpandedRepo] = useState<string | null>(null);
  const [repoFiles, setRepoFiles] = useState<Record<string, HfModelFile[]>>({});
  const [loadingFiles, setLoadingFiles] = useState<string | null>(null);
  const [downloadingFile, setDownloadingFile] = useState<string | null>(null);
  const [dlMsg, setDlMsg] = useState<string | null>(null);

  const search = useCallback(async () => {
    if (!query.trim()) return;
    setSearching(true); setSearchError(null); setResults([]); setExpandedRepo(null);
    try { setResults((await api.searchGgufModels(query.trim())).models ?? []); }
    catch (e) { setSearchError(String(e)); }
    finally { setSearching(false); }
  }, [query]);

  async function browseFiles(repoId: string) {
    if (expandedRepo === repoId) { setExpandedRepo(null); return; }
    setExpandedRepo(repoId);
    if (repoFiles[repoId]) return;
    setLoadingFiles(repoId);
    try { setRepoFiles((p) => ({ ...p, [repoId]: [] }));
      const res = await api.listHfModelFiles(repoId);
      setRepoFiles((p) => ({ ...p, [repoId]: res.files ?? [] }));
    } catch { /* empty */ } finally { setLoadingFiles(null); }
  }

  async function startDownload(file: HfModelFile) {
    setDownloadingFile(file.filename); setDlMsg(null);
    try {
      const res = await api.downloadModelFromUrl(file.url, "gguf", file.filename);
      setDlMsg(res.status === "already_downloaded" ? `${file.filename} already downloaded.` : `Download started: ${file.filename}`);
      onDownloadStarted();
    } catch (e) { setDlMsg(`Error: ${String(e)}`); }
    finally { setDownloadingFile(null); }
  }

  return (
    <div style={accordionSt.root}>
      <button style={accordionSt.header} onClick={() => { setOpen((v) => !v); if (!open && results.length === 0) search(); }}>
        <span style={accordionSt.headerLabel}><Download size={13} /> Browse HuggingFace</span>
        {open ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
      </button>

      {open && (
        <div style={accordionSt.body}>
          <div style={accordionSt.searchRow}>
            <div style={{ position: "relative", flex: 1 }}>
              <Search size={13} style={{ position: "absolute", left: 9, top: "50%", transform: "translateY(-50%)", color: "var(--color-text-tertiary)", pointerEvents: "none" }} />
              <input
                style={accordionSt.searchInput}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && search()}
                placeholder="Search GGUF models…"
                aria-label="Search HuggingFace GGUF models"
              />
            </div>
            <Button variant="primary" size="sm" onPress={search} isDisabled={searching || !state.serverOnline}>
              {searching ? "…" : "Search"}
            </Button>
          </div>

          {searchError && <p style={{ ...hint, color: "var(--color-destructive)" }}>{searchError}</p>}
          {dlMsg && <p style={{ ...hint, color: dlMsg.startsWith("Error") ? "var(--color-destructive)" : "var(--color-success)" }}>{dlMsg}</p>}

          {!searching && results.length === 0 && !searchError && (
            <p style={hint}>No results. Search for "gemma", "llama", or "mistral".</p>
          )}

          <div style={{ display: "flex", flexDirection: "column", gap: "var(--space-1)" }}>
            {results.map((model) => {
              const isExpanded = expandedRepo === model.id;
              const files = repoFiles[model.id] ?? [];
              return (
                <div key={model.id} style={accordionSt.repoCard}>
                  <div style={accordionSt.repoHeader}>
                    <div style={{ flex: 1, minWidth: 0 }}>
                      <a href={model.url} target="_blank" rel="noopener noreferrer" style={accordionSt.repoId}>{model.id}</a>
                      <div style={{ display: "flex", flexWrap: "wrap" as const, gap: "4px", marginTop: "2px" }}>
                        <span style={mlSt.badge}>↓ {model.downloads?.toLocaleString() ?? "?"}</span>
                        <span style={mlSt.badge}>♥ {model.likes ?? "?"}</span>
                        {model.tags?.slice(0, 2).map((tag) => <span key={tag} style={mlSt.badge}>{tag}</span>)}
                      </div>
                    </div>
                    <Button variant="outline" size="sm" onPress={() => browseFiles(model.id)} isDisabled={!state.serverOnline}>
                      {loadingFiles === model.id ? "…" : isExpanded ? "▲ Hide" : "▼ Files"}
                    </Button>
                  </div>
                  {isExpanded && (
                    <div style={accordionSt.fileList}>
                      {loadingFiles === model.id && <p style={{ ...hint, padding: "var(--space-2) var(--space-3)" }}>Loading files…</p>}
                      {!loadingFiles && files.length === 0 && <p style={{ ...hint, padding: "var(--space-2) var(--space-3)" }}>No .gguf files in this repo.</p>}
                      {files.map((file) => (
                        <div key={file.filename} style={accordionSt.fileRow}>
                          <code style={mlSt.name}>{file.filename}</code>
                          {file.size_mb != null && <span style={mlSt.badge}>{file.size_mb.toLocaleString()} MB</span>}
                          <Button
                            variant="primary" size="sm"
                            onPress={() => startDownload(file)}
                            isDisabled={downloadingFile === file.filename || !state.serverOnline}
                          >
                            <Download size={11} /> {downloadingFile === file.filename ? "Starting…" : "Download"}
                          </Button>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}

// ── Browse Llamafile GitHub Accordion ─────────────────────────

function BrowseGithubAccordion({ onDownloadStarted }: { onDownloadStarted: () => void }) {
  const state = useAppState();
  const [open, setOpen] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [loading, setLoading] = useState(false);
  const [releases, setReleases] = useState<LlamafileRelease[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [dlMsg, setDlMsg] = useState<string | null>(null);
  const [downloadingFile, setDownloadingFile] = useState<string | null>(null);

  async function load() {
    if (loaded) return;
    setLoading(true); setError(null);
    try {
      const res = await api.searchLlamafileModels();
      setReleases(res.models ?? []);
      setLoaded(true);
    } catch (e) { setError(String(e)); }
    finally { setLoading(false); }
  }

  async function startDownload(rel: LlamafileRelease) {
    setDownloadingFile(rel.name); setDlMsg(null);
    try {
      const res = await api.downloadModelFromUrl(rel.download_url, "llamafile", rel.name);
      setDlMsg(res.status === "already_downloaded" ? `${rel.name} already downloaded.` : `Download started: ${rel.name}`);
      onDownloadStarted();
    } catch (e) { setDlMsg(`Error: ${String(e)}`); }
    finally { setDownloadingFile(null); }
  }

  return (
    <div style={accordionSt.root}>
      <button style={accordionSt.header} onClick={() => { setOpen((v) => !v); if (!open) load(); }}>
        <span style={accordionSt.headerLabel}><Download size={13} /> Browse GitHub Releases</span>
        {open ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
      </button>
      {open && (
        <div style={accordionSt.body}>
          {loading && <p style={hint}>Fetching releases…</p>}
          {error && <p style={{ ...hint, color: "var(--color-destructive)" }}>{error}</p>}
          {dlMsg && <p style={{ ...hint, color: dlMsg.startsWith("Error") ? "var(--color-destructive)" : "var(--color-success)" }}>{dlMsg}</p>}
          {!loading && releases.length === 0 && !error && <p style={hint}>No llamafile releases found.</p>}
          <div style={{ display: "flex", flexDirection: "column", gap: "var(--space-1)" }}>
            {releases.map((rel) => (
              <div key={rel.name} style={accordionSt.fileRow}>
                <div style={{ flex: 1, minWidth: 0 }}>
                  <code style={mlSt.name}>{rel.name}</code>
                  <div style={{ display: "flex", gap: "4px", marginTop: "2px" }}>
                    <span style={mlSt.badge}>{rel.tag}</span>
                    {rel.size_mb != null && <span style={mlSt.badge}>{rel.size_mb.toLocaleString()} MB</span>}
                  </div>
                </div>
                <Button
                  variant="primary" size="sm"
                  onPress={() => startDownload(rel)}
                  isDisabled={downloadingFile === rel.name || !state.serverOnline}
                >
                  <Download size={11} /> {downloadingFile === rel.name ? "Starting…" : "Download"}
                </Button>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

const accordionSt: Record<string, React.CSSProperties> = {
  root: {
    border: "1px solid var(--color-border)",
    borderRadius: "var(--radius-md)",
    overflow: "hidden",
    background: "var(--color-bg)",
  },
  header: {
    display: "flex", alignItems: "center", justifyContent: "space-between",
    width: "100%", background: "rgba(23,22,22,0.02)", border: "none",
    padding: "var(--space-3) var(--space-4)", cursor: "pointer",
    color: "var(--color-text-secondary)", transition: "background var(--transition-fast)",
  },
  headerLabel: {
    display: "flex", alignItems: "center", gap: "var(--space-2)",
    fontSize: "var(--text-sm)", fontWeight: 600,
  },
  body: {
    display: "flex", flexDirection: "column" as const, gap: "var(--space-2)",
    padding: "var(--space-3) var(--space-4)",
    borderTop: "1px solid var(--color-border)",
  },
  searchRow: { display: "flex", gap: "var(--space-2)", alignItems: "center" },
  searchInput: {
    width: "100%", height: "32px", paddingLeft: "32px", paddingRight: "var(--space-3)",
    border: "1px solid var(--color-border-strong)", borderRadius: "var(--radius-md)",
    fontSize: "var(--text-sm)", fontFamily: "var(--font-body)",
    background: "var(--color-bg)", color: "var(--color-text)", outline: "none",
    boxSizing: "border-box" as const,
  },
  repoCard: {
    border: "1px solid var(--color-border)", borderRadius: "var(--radius-md)",
    overflow: "hidden", background: "rgba(23,22,22,0.01)",
  },
  repoHeader: {
    display: "flex", alignItems: "flex-start", justifyContent: "space-between",
    gap: "var(--space-3)", padding: "var(--space-2) var(--space-3)",
  },
  repoId: {
    fontWeight: 600, fontSize: "var(--text-xs)", color: "var(--color-accent)",
    textDecoration: "none", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" as const,
    display: "block",
  },
  fileList: { borderTop: "1px solid var(--color-border)", background: "rgba(23,22,22,0.02)" },
  fileRow: {
    display: "flex", alignItems: "center", justifyContent: "space-between",
    gap: "var(--space-3)", padding: "var(--space-2) var(--space-3)",
    borderBottom: "1px solid var(--color-border)",
  },
};

// ── Ollama Panel ──────────────────────────────────────────────

function OllamaPanel({
  models,
  modelsLoading,
  modelsError,
  activeRoles,
  onActivate,
  onDelete,
}: {
  models: ModelEntry[];
  modelsLoading: boolean;
  modelsError: string | null;
  activeRoles: ModelActiveRoles | null;
  onActivate: (provider: string, name: string, role: string) => void;
  onDelete: (provider: string, name: string) => void;
}) {
  const state = useAppState();
  const [ollamaModels, setOllamaModels] = useState<OllamaModel[]>([]);
  const [ollamaError, setOllamaError] = useState<string | null>(null);
  const [ollamaLoading, setOllamaLoading] = useState(true);
  const [pullInput, setPullInput] = useState("");
  const [pulling, setPulling] = useState(false);
  const [pullMsg, setPullMsg] = useState<string | null>(null);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  async function loadOllamaModels() {
    setOllamaLoading(true);
    try {
      const res = await api.listOllamaModels();
      setOllamaModels(res.models ?? []);
      setOllamaError(res.error ?? null);
    } catch (e) { setOllamaError(String(e)); }
    finally { setOllamaLoading(false); }
  }

  useEffect(() => {
    loadOllamaModels();
    return () => { if (pollRef.current) clearInterval(pollRef.current); };
  }, []);

  async function handlePull() {
    if (!pullInput.trim()) return;
    setPulling(true); setPullMsg(null);
    try {
      await api.pullOllamaModel(pullInput.trim());
      setPullMsg(`Pulling "${pullInput.trim()}"… This may take a few minutes.`);
      // Poll for new models every 5s while pulling
      if (!pollRef.current) {
        pollRef.current = setInterval(loadOllamaModels, 5000);
        setTimeout(() => { if (pollRef.current) { clearInterval(pollRef.current); pollRef.current = null; } }, 120_000);
      }
    } catch (e) { setPullMsg(`Error: ${String(e)}`); }
    finally { setPulling(false); }
  }

  const isRunning = !ollamaError || !ollamaError.toLowerCase().includes("not running");
  const ollamaModelEntries: ModelEntry[] = ollamaModels.map((om) => ({
    id: `ollama/${om.name}`,
    provider: "ollama",
    name: om.name,
    display_name: om.name,
    is_active: false,
    ram_estimate_mb: om.size != null ? Math.round(om.size / (1024 * 1024)) : undefined,
  }));

  const allOllamaModels = [
    ...models, // from registry (may include ollama entries already scanned)
    ...ollamaModelEntries.filter((om) => !models.some((m) => m.name === om.name && m.provider === "ollama")),
  ];

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "var(--space-3)" }}>
      {/* Status bar */}
      <div style={ollamaSt.statusBar}>
        <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
          <span style={{ ...ollamaSt.dot, background: ollamaLoading ? "#aaa" : isRunning ? "var(--color-success)" : "#e5a000" }} />
          <span style={ollamaSt.statusText}>
            {ollamaLoading ? "Checking Ollama…" : isRunning ? `Ollama running — ${ollamaModels.length} model${ollamaModels.length !== 1 ? "s" : ""}` : "Ollama not running"}
          </span>
        </div>
        <div style={{ display: "flex", gap: "var(--space-2)" }}>
          {!isRunning && !ollamaLoading && (
            <Button variant="outline" size="sm" onPress={() => {
              setPullMsg("Run `ollama serve` in a terminal to start Ollama, then refresh.");
            }}>
              <Play size={12} /> How to start
            </Button>
          )}
          <Button variant="ghost" size="sm" onPress={loadOllamaModels} isDisabled={ollamaLoading}>
            <RefreshCw size={12} />
          </Button>
        </div>
      </div>

      {pullMsg && <p style={{ ...hint, color: pullMsg.startsWith("Error") ? "var(--color-destructive)" : pullMsg.startsWith("Run") ? "var(--color-text-secondary)" : "var(--color-success)" }}>{pullMsg}</p>}

      {/* Pull row */}
      <div style={ollamaSt.pullRow}>
        <input
          style={ollamaSt.pullInput}
          value={pullInput}
          onChange={(e) => setPullInput(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && handlePull()}
          placeholder="Pull model, e.g. llama3.2:3b"
          aria-label="Ollama model to pull"
          disabled={!state.serverOnline || !isRunning}
        />
        <Button
          variant="primary" size="sm"
          onPress={handlePull}
          isDisabled={!pullInput.trim() || pulling || !state.serverOnline || !isRunning}
        >
          {pulling ? "Starting…" : "Pull model"}
        </Button>
      </div>

      {/* Model list */}
      <ModelList
        models={allOllamaModels}
        loading={modelsLoading && ollamaLoading}
        error={modelsError}
        activeRoles={activeRoles}
        availableRoles={["chat", "think", "task"]}
        onActivate={onActivate}
        onDelete={onDelete}
        emptyMessage={isRunning ? "No Ollama models found. Pull a model above." : "Ollama is not running. Start it to see available models."}
      />
    </div>
  );
}

const ollamaSt: Record<string, React.CSSProperties> = {
  statusBar: {
    display: "flex", alignItems: "center", justifyContent: "space-between",
    padding: "var(--space-3) var(--space-4)",
    background: "rgba(23,22,22,0.02)", border: "1px solid var(--color-border)",
    borderRadius: "var(--radius-md)",
  },
  dot: { width: "8px", height: "8px", borderRadius: "50%", flexShrink: 0 },
  statusText: { fontSize: "var(--text-sm)", color: "var(--color-text-secondary)", fontWeight: 500 },
  pullRow: { display: "flex", gap: "var(--space-2)", alignItems: "center" },
  pullInput: {
    flex: 1, height: "34px", padding: "0 var(--space-3)",
    border: "1px solid var(--color-border-strong)", borderRadius: "var(--radius-md)",
    fontSize: "var(--text-sm)", fontFamily: "var(--font-body)",
    background: "var(--color-bg)", color: "var(--color-text)", outline: "none",
  },
};

// ── LLM Tab ───────────────────────────────────────────────────

type LlmProvider = "gguf" | "llamafile" | "ollama";

function LlmTab({
  models,
  modelsLoading,
  modelsError,
  activeRoles,
  onActivate,
  onDelete,
  onDownloadStarted,
  onScanModels,
}: {
  models: ModelEntry[];
  modelsLoading: boolean;
  modelsError: string | null;
  activeRoles: ModelActiveRoles | null;
  onActivate: (provider: string, name: string, role: string) => void;
  onDelete: (provider: string, name: string) => void;
  onDownloadStarted: () => void;
  onScanModels: () => void;
}) {
  const [provider, setProvider] = useState<LlmProvider>("gguf");
  const PROVIDERS: Array<{ key: LlmProvider; label: string }> = [
    { key: "gguf", label: "GGUF" },
    { key: "llamafile", label: "Llamafile" },
    { key: "ollama", label: "Ollama" },
  ];

  const ggufModels = models.filter((m) => m.provider === "gguf");
  const llamafileModels = models.filter((m) => m.provider === "llamafile");
  const ollamaRegistryModels = models.filter((m) => m.provider === "ollama");

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "var(--space-4)" }}>
      {/* Provider pills */}
      <div style={llmSt.pills}>
        {PROVIDERS.map(({ key, label }) => (
          <button
            key={key}
            style={{
              ...llmSt.pill,
              background: provider === key ? "rgba(140,82,255,0.1)" : "transparent",
              borderColor: provider === key ? "var(--color-accent)" : "var(--color-border)",
              color: provider === key ? "var(--color-accent)" : "var(--color-text-secondary)",
              fontWeight: provider === key ? 700 : 500,
            }}
            onClick={() => setProvider(key)}
          >
            {label}
          </button>
        ))}
        <div style={{ flex: 1 }} />
        <Button variant="ghost" size="sm" onPress={onScanModels} aria-label="Scan for models">
          <RefreshCw size={12} /> Scan
        </Button>
      </div>

      {/* GGUF panel */}
      {provider === "gguf" && (
        <div style={{ display: "flex", flexDirection: "column", gap: "var(--space-3)" }}>
          <ModelList
            models={ggufModels}
            loading={modelsLoading}
            error={modelsError}
            activeRoles={activeRoles}
            availableRoles={["chat", "think", "task"]}
            onActivate={onActivate}
            onDelete={onDelete}
            emptyMessage="No GGUF models found. Download one below."
          />
          <BrowseHfAccordion onDownloadStarted={onDownloadStarted} />
        </div>
      )}

      {/* Llamafile panel */}
      {provider === "llamafile" && (
        <div style={{ display: "flex", flexDirection: "column", gap: "var(--space-3)" }}>
          <ModelList
            models={llamafileModels}
            loading={modelsLoading}
            error={modelsError}
            activeRoles={activeRoles}
            availableRoles={["chat", "think", "task"]}
            onActivate={onActivate}
            onDelete={onDelete}
            emptyMessage="No Llamafile models found. Download one below."
          />
          <BrowseGithubAccordion onDownloadStarted={onDownloadStarted} />
        </div>
      )}

      {/* Ollama panel */}
      {provider === "ollama" && (
        <OllamaPanel
          models={ollamaRegistryModels}
          modelsLoading={modelsLoading}
          modelsError={modelsError}
          activeRoles={activeRoles}
          onActivate={onActivate}
          onDelete={onDelete}
        />
      )}
    </div>
  );
}

const llmSt: Record<string, React.CSSProperties> = {
  pills: { display: "flex", alignItems: "center", gap: "var(--space-2)" },
  pill: {
    height: "30px", padding: "0 var(--space-3)",
    border: "1px solid", borderRadius: "var(--radius-md)",
    fontSize: "var(--text-sm)", fontFamily: "var(--font-body)",
    cursor: "pointer", transition: "all var(--transition-fast)",
    whiteSpace: "nowrap" as const,
  },
};

// ── Category Tabs ─────────────────────────────────────────────

type Category = "llm" | "asr" | "tts" | "face";

const CATEGORIES: Array<{ key: Category; label: string; icon: React.ReactNode; color: string }> = [
  { key: "llm",  label: "LLM",  icon: <Brain size={14} />,     color: CAT_COLOR.llm  },
  { key: "asr",  label: "ASR",  icon: <Mic size={14} />,       color: CAT_COLOR.asr  },
  { key: "tts",  label: "TTS",  icon: <Volume2 size={14} />,   color: CAT_COLOR.tts  },
  { key: "face", label: "Face", icon: <ScanFace size={14} />,  color: CAT_COLOR.face },
];

// ── Face Recognition Panel ───────────────────────────────────
//
// Read-only status for the three face models (ArcFace R50 + SCRFD 10G +
// Silent-Face PAD). pond-server downloads them automatically on first
// boot when built with `--features face-onnx`, so there is no per-model
// "Download" button — operators just watch progress here. When the
// feature is disabled the card surfaces the rebuild instruction.
function FacePanel() {
  const [data, setData]       = useState<FaceModelsResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError]     = useState<string | null>(null);

  const reload = useCallback(async () => {
    setLoading(true); setError(null);
    try { setData(await api.listFaceModels()); }
    catch (e) { setError(String(e)); }
    finally { setLoading(false); }
  }, []);

  useEffect(() => { reload(); }, [reload]);

  const installed = data?.models.filter(m => m.downloaded).length ?? 0;
  const total     = data?.models.length ?? 0;

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "var(--space-3)" }}>
      <div style={rootSt.sectionHeader}>
        <ScanFace size={14} style={{ color: CAT_COLOR.face }} />
        <span style={{ ...rootSt.sectionTitle, color: CAT_COLOR.face }}>Face Recognition</span>
        {data && (
          <span style={mlSt.badge}>
            {data.feature_enabled ? `${installed}/${total} ready` : "feature disabled"}
          </span>
        )}
        <div style={{ flex: 1 }} />
        <Button variant="ghost" size="sm" onPress={reload} isDisabled={loading}>
          <RefreshCw size={12} /> Refresh
        </Button>
      </div>

      {loading && <p style={hint}>Loading…</p>}
      {error && <p style={{ ...hint, color: "var(--color-destructive)" }}>{error}</p>}

      {data && !data.feature_enabled && (
        <p style={hint}>
          Face recognition is disabled in this build. Rebuild pond-server with
          {" "}<code style={inlineCode}>--features face-onnx</code>{" "}
          to enable per-user identification.
        </p>
      )}

      {data && data.models.length > 0 && (
        <div style={{ display: "flex", flexDirection: "column", gap: "var(--space-2)" }}>
          {data.models.map(m => (
            <div
              key={m.name}
              style={{
                ...mlSt.row,
                borderLeftColor: m.downloaded ? CAT_COLOR.face : "transparent",
              }}
            >
              <div style={mlSt.left}>
                <div style={mlSt.nameRow}>
                  <span style={mlSt.name}>{m.label}</span>
                  <span style={mlSt.badge}>{m.role}</span>
                  {m.downloaded ? (
                    <span style={{ ...mlSt.badge, color: "var(--color-success)" }}>
                      {m.size_mb != null ? `${m.size_mb} MB` : "ready"}
                    </span>
                  ) : (
                    <span style={{ ...mlSt.badge, color: "#e5a000" }}>
                      missing · ~{m.expected_mb} MB
                    </span>
                  )}
                </div>
                <span style={mlSt.sub}>{m.name}</span>
              </div>
            </div>
          ))}
        </div>
      )}

      {data?.models_dir && (
        <p style={{ ...hint, fontFamily: "var(--font-mono)", fontSize: "var(--text-xs)", opacity: 0.6 }}>
          {data.models_dir}
        </p>
      )}

      <p style={hint}>
        Models auto-download on first server boot. The buffalo_l zip ships ArcFace R50 +
        SCRFD 10G in a single ~281 MB archive; Silent-Face PAD is ~2 MB. Once installed,
        use the <strong>Face Enrollment</strong> page (web UI) to register household members.
      </p>
    </div>
  );
}

// ── Memory Status Bar ─────────────────────────────────────────

function MemoryStatusBar({ status }: { status: ModelMemoryStatus | null }) {
  if (!status) return null;
  const { total_mb, available_for_llm_mb, loaded_model } = status;
  return (
    <div style={{
      background: "var(--color-bg)",
      border: "1px solid var(--color-border)",
      borderRadius: "var(--radius-lg)",
      padding: "var(--space-3) var(--space-4)",
      display: "flex",
      alignItems: "center",
      gap: "var(--space-4)",
      flexWrap: "wrap" as const,
    }}>
      <span style={{ fontFamily: "var(--font-display)", fontWeight: 700, fontSize: "var(--text-xs)", color: "var(--color-text-secondary)", textTransform: "uppercase" as const, letterSpacing: "0.06em" }}>
        System Memory
      </span>
      {total_mb > 0 ? (
        <>
          <span style={{ fontFamily: "var(--font-mono)", fontSize: "var(--text-xs)", color: "var(--color-text)" }}>
            {total_mb.toLocaleString()} MB total · {available_for_llm_mb.toLocaleString()} MB available
          </span>
          {loaded_model && (
            <span style={{ fontFamily: "var(--font-mono)", fontSize: "var(--text-xs)", color: "var(--color-accent)" }}>
              Hot: {loaded_model}
            </span>
          )}
        </>
      ) : (
        <span style={{ fontFamily: "var(--font-mono)", fontSize: "var(--text-xs)", color: "var(--color-text-tertiary)", fontStyle: "italic" }}>
          Managed externally
        </span>
      )}
    </div>
  );
}

// ── Main Models Component ─────────────────────────────────────

export function Models() {
  const [category, setCategory] = useState<Category>("llm");
  const [activeRoles, setActiveRoles] = useState<ModelActiveRoles | null>(null);
  const [rolesLoading, setRolesLoading] = useState(false);
  const [models, setModels] = useState<ModelEntry[]>([]);
  const [modelsLoading, setModelsLoading] = useState(true);
  const [modelsError, setModelsError] = useState<string | null>(null);
  const [downloads, setDownloads] = useState<DownloadEntry[]>([]);
  const [actionMsg, setActionMsg] = useState<{ text: string; ok: boolean } | null>(null);
  const [memoryStatus, setMemoryStatus] = useState<ModelMemoryStatus | null>(null);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const loadRoles = useCallback(async () => {
    setRolesLoading(true);
    try { setActiveRoles(await api.getActiveRoles()); }
    catch { /* non-fatal */ }
    finally { setRolesLoading(false); }
  }, []);

  const loadModels = useCallback(async () => {
    setModelsLoading(true); setModelsError(null);
    try { setModels(await api.listModels()); }
    catch (e) { setModelsError(String(e)); }
    finally { setModelsLoading(false); }
  }, []);

  const loadDownloads = useCallback(async () => {
    try { setDownloads((await api.getDownloadProgress()).downloads ?? []); }
    catch { /* non-fatal */ }
  }, []);

  const startDownloadPoll = useCallback(() => {
    if (pollRef.current) return;
    pollRef.current = setInterval(async () => {
      await loadDownloads();
      const { downloads: dl } = await api.getDownloadProgress().catch(() => ({ downloads: [] as DownloadEntry[] }));
      if (dl.every((d) => d.status === "done" || d.status === "error") && pollRef.current) {
        clearInterval(pollRef.current);
        pollRef.current = null;
        loadModels();
      }
    }, 3000);
  }, [loadDownloads, loadModels]);

  useEffect(() => {
    loadRoles(); loadModels(); loadDownloads();
    api.getMemoryStatus().then(setMemoryStatus).catch(() => {/* non-fatal */});
    return () => { if (pollRef.current) { clearInterval(pollRef.current); pollRef.current = null; } };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  function flash(text: string, ok = true) {
    setActionMsg({ text, ok });
    setTimeout(() => setActionMsg(null), 3000);
  }

  async function handleActivate(provider: string, name: string, role: string) {
    try {
      await api.activateModel(provider, name, role);
      flash(`${name} set as ${role} model.`);
      await loadRoles();
    } catch (e) { flash(String(e), false); }
  }

  async function handleDelete(provider: string, name: string) {
    if (!confirm(`Delete "${name}"? This removes the file from disk.`)) return;
    try {
      await api.deleteModel(provider, name);
      flash(`${name} deleted.`);
      await loadModels();
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        flash(`Cannot delete "${name}" — it is currently assigned to an active role. Deactivate it first.`, false);
      } else {
        flash(String(e), false);
      }
    }
  }

  async function handleScan() {
    try {
      const res = await api.scanModels();
      flash(`Scanned: ${res.found} model${res.found !== 1 ? "s" : ""} found.`);
      await loadModels();
    } catch (e) { flash(String(e), false); }
  }

  const asrModels = models.filter((m) => m.provider === "whisper");
  const ttsModels = models.filter((m) => m.provider === "tts" || m.provider === "tts_piper" || m.provider === "tts_http");

  return (
    <div style={rootSt.root}>
      {/* Active Roles Banner */}
      <ActiveRolesBanner
        roles={activeRoles}
        onRefresh={loadRoles}
        loading={rolesLoading}
        onNavigate={(cat) => setCategory(cat)}
      />

      {/* Memory Status */}
      <MemoryStatusBar status={memoryStatus} />

      {/* Download Progress */}
      <DownloadProgress downloads={downloads} onScanModels={handleScan} />

      {/* Action feedback */}
      {actionMsg && (
        <p style={{ ...hint, color: actionMsg.ok ? "var(--color-success)" : "var(--color-destructive)" }}>
          {actionMsg.text}
        </p>
      )}

      {/* Category tabs */}
      <div style={rootSt.catTabs}>
        {CATEGORIES.map(({ key, label, icon, color }) => (
          <button
            key={key}
            style={{
              ...rootSt.catTab,
              borderBottomColor: category === key ? color : "transparent",
              color: category === key ? color : "var(--color-text-secondary)",
              fontWeight: category === key ? 700 : 500,
            }}
            onClick={() => setCategory(key)}
          >
            <span style={{ color: category === key ? color : "var(--color-text-tertiary)" }}>{icon}</span>
            {label}
          </button>
        ))}
      </div>

      {/* Tab content */}
      {category === "llm" && (
        <LlmTab
          models={models}
          modelsLoading={modelsLoading}
          modelsError={modelsError}
          activeRoles={activeRoles}
          onActivate={handleActivate}
          onDelete={handleDelete}
          onDownloadStarted={() => { startDownloadPoll(); loadDownloads(); }}
          onScanModels={handleScan}
        />
      )}

      {category === "asr" && (
        <div style={{ display: "flex", flexDirection: "column", gap: "var(--space-3)" }}>
          <div style={rootSt.sectionHeader}>
            <Mic size={14} style={{ color: CAT_COLOR.asr }} />
            <span style={{ ...rootSt.sectionTitle, color: CAT_COLOR.asr }}>Automatic Speech Recognition</span>
          </div>
          <ModelList
            models={asrModels}
            loading={modelsLoading}
            error={modelsError}
            activeRoles={activeRoles}
            availableRoles={["asr"]}
            onActivate={handleActivate}
            onDelete={handleDelete}
            emptyMessage="No Whisper models found. Run a model scan or download from HuggingFace."
          />
          <p style={hint}>Whisper models power voice-to-text transcription. Place <code style={inlineCode}>.bin</code> files in <code style={inlineCode}>models/whisper/</code> and click Scan in the LLM tab.</p>
        </div>
      )}

      {category === "face" && <FacePanel />}

      {category === "tts" && (
        <div style={{ display: "flex", flexDirection: "column", gap: "var(--space-3)" }}>
          <div style={rootSt.sectionHeader}>
            <Volume2 size={14} style={{ color: CAT_COLOR.tts }} />
            <span style={{ ...rootSt.sectionTitle, color: CAT_COLOR.tts }}>Text-to-Speech</span>
          </div>
          <ModelList
            models={ttsModels}
            loading={modelsLoading}
            error={modelsError}
            activeRoles={activeRoles}
            availableRoles={["tts"]}
            onActivate={handleActivate}
            onDelete={handleDelete}
            emptyMessage="No TTS models found. Place Piper .onnx files in models/tts/ and scan."
          />
          <p style={hint}>TTS models power the voice output. Piper voices use <code style={inlineCode}>.onnx</code> + <code style={inlineCode}>.json</code> pairs in <code style={inlineCode}>models/tts/</code>.</p>
        </div>
      )}
    </div>
  );
}

const hint: React.CSSProperties = { color: "var(--color-text-tertiary)", fontSize: "var(--text-sm)", margin: 0 };
const inlineCode: React.CSSProperties = { fontFamily: "var(--font-mono)", fontSize: "0.85em", background: "rgba(23,22,22,0.06)", padding: "1px 5px", borderRadius: "4px" };

const rootSt: Record<string, React.CSSProperties> = {
  root: { display: "flex", flexDirection: "column", gap: "var(--space-4)", maxWidth: "var(--content-max-width)" },
  catTabs: {
    display: "flex", gap: 0,
    borderBottom: "2px solid var(--color-border)",
  },
  catTab: {
    display: "inline-flex", alignItems: "center", gap: "var(--space-2)",
    height: "40px", padding: "0 var(--space-4)",
    border: "none", borderBottom: "3px solid",
    background: "transparent", cursor: "pointer",
    fontSize: "var(--text-sm)", fontFamily: "var(--font-body)",
    transition: "all var(--transition-fast)",
    marginBottom: "-2px",
  },
  sectionHeader: { display: "flex", alignItems: "center", gap: "var(--space-2)" },
  sectionTitle: {
    fontFamily: "var(--font-display)", fontWeight: 700,
    fontSize: "var(--text-sm)", textTransform: "uppercase" as const, letterSpacing: "0.06em",
  },
};
