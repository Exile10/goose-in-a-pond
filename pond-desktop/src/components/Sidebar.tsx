import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { SIDEBAR_GROUPS, type GuiSection } from "../desktopState";
import { useAppState, useAppDispatch } from "../state/AppContext";
import logoSrc from "../assets/logo.png";
import {
  Home, MessageSquare, Monitor, Clock, BrainCircuit, Zap,
  Cpu, FileText, Settings, User,
  ChevronRight, ChevronLeft,
  Mic, Layers, ScanFace,
} from "lucide-react";

// ── Icon map ─────────────────────────────────────────────────────────────────

const NAV_ICONS: Record<string, React.ElementType> = {
  home: Home,
  chat: MessageSquare,
  devices: Monitor,
  clock: Clock,
  memory: BrainCircuit,
  skills: Zap,
  model: Cpu,
  prompt: FileText,
  settings: Settings,
  face: ScanFace,
  agent: User,
};

function NavIcon({ name }: { name: string }) {
  const Icon = NAV_ICONS[name];
  if (!Icon) return <span style={{ width: 16, height: 16, display: "inline-block" }} />;
  return <Icon size={16} />;
}

// ── Sidebar Component ─────────────────────────────────────────────────────────

export function Sidebar() {
  const state = useAppState();
  const dispatch = useAppDispatch();

  // Initialize from localStorage; auto-collapse below 960px only if no explicit preference set
  const [collapsed, setCollapsed] = useState<boolean>(
    () => localStorage.getItem("pond_sidebar_collapsed") === "true"
  );

  // Auto-collapse on narrow windows (only when no explicit user preference)
  useEffect(() => {
    function onResize() {
      if (window.innerWidth < 960 && !localStorage.getItem("pond_sidebar_collapsed")) {
        setCollapsed(true);
      }
    }
    window.addEventListener("resize", onResize);
    onResize(); // check on mount
    return () => window.removeEventListener("resize", onResize);
  }, []);

  function toggleCollapsed() {
    const next = !collapsed;
    setCollapsed(next);
    localStorage.setItem("pond_sidebar_collapsed", String(next));
  }

  function navigate(section: GuiSection) {
    dispatch({ type: "SET_SECTION", payload: section });
  }

  async function switchToVoice() {
    dispatch({ type: "SET_MODE", payload: "voice" });
  }

  async function switchToCanvas() {
    dispatch({ type: "SET_MODE", payload: "canvas" });
    try { await invoke("show_canvas"); } catch { /* ignore */ }
  }

  const sidebarStyle: React.CSSProperties = {
    width: collapsed ? "var(--sidebar-width-collapsed)" : "var(--sidebar-width)",
    minWidth: collapsed ? "var(--sidebar-width-collapsed)" : "var(--sidebar-width)",
    transition: "var(--sidebar-transition)",
    height: "100%",
    background: "var(--color-sidebar)",
    borderRight: "1px solid var(--color-border)",
    display: "flex",
    flexDirection: "column",
    overflow: "hidden",
    flexShrink: 0,
  };

  return (
    <aside style={sidebarStyle} aria-label="Navigation">
      {/* Brand */}
      <div style={{ ...styles.brand, ...(collapsed ? styles.brandCollapsed : {}) }}>
        <img src={logoSrc} alt="" style={styles.brandLogo} aria-hidden="true" />
        {!collapsed && <span style={styles.brandName}>Goose In A Pond</span>}
      </div>

      <hr style={styles.divider} />

      {/* Nav groups */}
      <nav style={styles.nav}>
        {SIDEBAR_GROUPS.map((group, gi) => (
          <div key={gi} style={styles.group}>
            {group.label && !collapsed && (
              <p style={styles.groupLabel}>{group.label}</p>
            )}
            {group.sections.map((item) => {
              const active = state.section === item.section;
              return (
                <button
                  key={item.section}
                  style={{
                    ...styles.navItem,
                    ...(active ? styles.navItemActive : {}),
                    ...(collapsed ? styles.navItemCollapsed : {}),
                  }}
                  onClick={() => navigate(item.section)}
                  aria-current={active ? "page" : undefined}
                  title={collapsed ? item.label : undefined}
                >
                  <span style={{ color: active ? "var(--color-accent)" : "var(--color-text-secondary)", flexShrink: 0 }}>
                    <NavIcon name={item.icon} />
                  </span>
                  {!collapsed && (
                    <span style={{ color: active ? "var(--color-accent)" : "var(--color-text)" }}>
                      {item.label}
                    </span>
                  )}
                </button>
              );
            })}
          </div>
        ))}
      </nav>

      {/* Footer */}
      <div style={styles.footer}>
        <hr style={styles.divider} />

        {/* Server status */}
        <div style={{ ...styles.serverStatus, justifyContent: collapsed ? "center" : "flex-start" }}>
          <span
            style={{
              ...styles.statusDot,
              background: state.serverOnline ? "var(--color-success)" : "var(--color-neutral)",
            }}
          />
          {!collapsed && (
            <span style={styles.statusText}>
              {state.serverOnline ? "Connected" : state.serverStarting ? "Starting…" : "Offline"}
            </span>
          )}
        </div>

        {/* Mode buttons */}
        <div style={{ ...styles.modeButtons, gap: collapsed ? "4px" : "4px", flexDirection: collapsed ? "column" : "row" }}>
          <button
            style={{ ...styles.modeBtn, flex: collapsed ? undefined : 1 }}
            onClick={switchToVoice}
            title="Switch to Voice mode"
            aria-label="Voice mode"
          >
            {collapsed ? <Mic size={14} /> : <><Mic size={14} /> Voice</>}
          </button>
          <button
            style={{ ...styles.modeBtn, flex: collapsed ? undefined : 1 }}
            onClick={switchToCanvas}
            title="Open Canvas overlay"
            aria-label="Canvas mode"
          >
            {collapsed ? <Layers size={14} /> : <><Layers size={14} /> Canvas</>}
          </button>
        </div>

        {/* Collapse/expand chevron toggle */}
        <button
          style={styles.chevronBtn}
          onClick={toggleCollapsed}
          title={collapsed ? "Expand sidebar" : "Collapse sidebar"}
          aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"}
        >
          {collapsed ? <ChevronRight size={12} /> : <ChevronLeft size={12} />}
        </button>
      </div>
    </aside>
  );
}

const styles: Record<string, React.CSSProperties> = {
  brand: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    padding: "16px 12px 12px",
    userSelect: "none",
    overflow: "hidden",
    flexShrink: 0,
  },
  brandCollapsed: {
    justifyContent: "center",
    padding: "12px 10px 10px",
  },
  brandLogo: {
    width: "64px",
    height: "64px",
    objectFit: "contain",
    flexShrink: 0,
  },
  brandName: {
    fontFamily: "var(--font-display)",
    fontWeight: 700,
    fontSize: "13px",
    color: "var(--color-text)",
    lineHeight: "1.3",
    whiteSpace: "nowrap",
    overflow: "hidden",
  },
  divider: {
    border: "none",
    borderTop: "1px solid var(--color-border)",
    margin: 0,
    flexShrink: 0,
  },
  nav: {
    flex: 1,
    overflowY: "auto",
    overflowX: "hidden",
    padding: "8px 0",
    display: "flex",
    flexDirection: "column",
    gap: "2px",
  },
  group: {
    padding: "0 6px",
    marginBottom: "4px",
  },
  groupLabel: {
    fontFamily: "var(--font-display)",
    fontWeight: 600,
    fontSize: "var(--text-xs)",
    color: "var(--color-text-tertiary)",
    letterSpacing: "0.06em",
    padding: "8px 6px 4px",
    margin: 0,
    textTransform: "uppercase",
    whiteSpace: "nowrap",
    overflow: "hidden",
  },
  navItem: {
    width: "100%",
    display: "flex",
    alignItems: "center",
    justifyContent: "flex-start",
    gap: "8px",
    height: "var(--row-height-sm)",
    padding: "0 8px",
    borderRadius: "var(--radius-md)",
    background: "transparent",
    fontFamily: "var(--font-body)",
    fontSize: "var(--text-base)",
    fontWeight: 500,
    cursor: "pointer",
    border: "none",
    textAlign: "left",
    transition: "background var(--transition-fast)",
    userSelect: "none",
    whiteSpace: "nowrap",
    overflow: "hidden",
  },
  navItemCollapsed: {
    justifyContent: "center",
    padding: "0",
  },
  navItemActive: {
    background: "var(--color-accent-soft)",
    fontWeight: 600,
  },
  footer: {
    padding: "0 0 8px",
    display: "flex",
    flexDirection: "column",
    gap: "8px",
    flexShrink: 0,
  },
  serverStatus: {
    display: "flex",
    alignItems: "center",
    gap: "6px",
    padding: "8px 12px 4px",
    overflow: "hidden",
  },
  statusDot: {
    width: "7px",
    height: "7px",
    borderRadius: "50%",
    flexShrink: 0,
  },
  statusText: {
    fontFamily: "var(--font-body)",
    fontSize: "var(--text-sm)",
    color: "var(--color-text-secondary)",
    whiteSpace: "nowrap",
    overflow: "hidden",
  },
  modeButtons: {
    display: "flex",
    padding: "0 8px",
  },
  modeBtn: {
    height: "28px",
    fontSize: "11px",
    fontFamily: "var(--font-body)",
    fontWeight: 500,
    border: "1px solid var(--color-border-strong)",
    borderRadius: "var(--radius-md)",
    background: "transparent",
    color: "var(--color-text-secondary)",
    cursor: "pointer",
    transition: "background var(--transition-fast), color var(--transition-fast)",
    whiteSpace: "nowrap",
    padding: "0 6px",
    display: "flex",
    alignItems: "center",
    gap: "4px",
  },
  chevronBtn: {
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    height: "28px",
    background: "transparent",
    border: "none",
    cursor: "pointer",
    color: "var(--color-text-tertiary)",
    borderRadius: "var(--radius-sm)",
    margin: "0 8px",
    width: "calc(100% - 16px)",
    transition: "color var(--transition-fast), background var(--transition-fast)",
  },
};
