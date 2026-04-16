import { type JSX } from "react";
import { invoke } from "@tauri-apps/api/core";
import { SIDEBAR_GROUPS, type GuiSection } from "../desktopState";
import { useAppState, useAppDispatch } from "../state/AppContext";
import logoSrc from "../assets/logo.png";

// ── Inline SVG Icons ─────────────────────────────────────────────────────────

const icons: Record<string, JSX.Element> = {
  home: (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
      <path d="M8 1.5L2 6.5V14h4v-3.5h4V14h4V6.5L8 1.5z" stroke="currentColor" strokeWidth="1.4" strokeLinejoin="round" fill="none"/>
    </svg>
  ),
  chat: (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
      <path d="M13.5 2.5H2.5a1 1 0 00-1 1v7a1 1 0 001 1H5l3 2.5L11 11.5h2.5a1 1 0 001-1v-7a1 1 0 00-1-1z" stroke="currentColor" strokeWidth="1.4" strokeLinejoin="round" fill="none"/>
    </svg>
  ),
  devices: (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
      <rect x="1.5" y="3.5" width="9" height="7" rx="1" stroke="currentColor" strokeWidth="1.4" fill="none"/>
      <path d="M10.5 6.5h3a1 1 0 011 1v3a1 1 0 01-1 1h-3" stroke="currentColor" strokeWidth="1.4" fill="none"/>
      <circle cx="13" cy="8" r="0.75" fill="currentColor"/>
    </svg>
  ),
  clock: (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
      <circle cx="8" cy="8" r="6" stroke="currentColor" strokeWidth="1.4" fill="none"/>
      <path d="M8 5v3l2 1.5" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round"/>
    </svg>
  ),
  memory: (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
      <rect x="2" y="4" width="12" height="8" rx="1.5" stroke="currentColor" strokeWidth="1.4" fill="none"/>
      <path d="M5 4V3M8 4V3M11 4V3M5 12v1M8 12v1M11 12v1" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round"/>
      <path d="M5 8h6" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round"/>
    </svg>
  ),
  skills: (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
      <path d="M8 2l1.5 3L13 5.5l-2.5 2.5.6 3.5L8 10l-3.1 1.5.6-3.5L3 5.5l3.5-.5L8 2z" stroke="currentColor" strokeWidth="1.4" strokeLinejoin="round" fill="none"/>
    </svg>
  ),
  model: (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
      <circle cx="8" cy="4" r="2" stroke="currentColor" strokeWidth="1.4" fill="none"/>
      <circle cx="3" cy="12" r="2" stroke="currentColor" strokeWidth="1.4" fill="none"/>
      <circle cx="13" cy="12" r="2" stroke="currentColor" strokeWidth="1.4" fill="none"/>
      <path d="M8 6v2M6.3 10.7L4.5 10M9.7 10.7l1.8-.7" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round"/>
    </svg>
  ),
  prompt: (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
      <path d="M3 4h10M3 7.5h7M3 11h5" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round"/>
    </svg>
  ),
  settings: (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
      <circle cx="8" cy="8" r="2.5" stroke="currentColor" strokeWidth="1.4" fill="none"/>
      <path d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round"/>
    </svg>
  ),
  agent: (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
      <circle cx="8" cy="7" r="3.5" stroke="currentColor" strokeWidth="1.4" fill="none"/>
      <path d="M2.5 14c0-2.5 2.5-4 5.5-4s5.5 1.5 5.5 4" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" fill="none"/>
    </svg>
  ),
};

function NavIcon({ name }: { name: string }) {
  return icons[name] ?? <span style={{ width: 16, height: 16, display: "inline-block" }} />;
}

// ── Sidebar Component ─────────────────────────────────────────────────────────

export function Sidebar() {
  const state = useAppState();
  const dispatch = useAppDispatch();

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

  return (
    <aside style={styles.sidebar} aria-label="Navigation">
      {/* Brand */}
      <div style={styles.brand}>
        <img src={logoSrc} alt="" style={styles.brandLogo} aria-hidden="true" />
        <span style={styles.brandName}>Goose In A Pond</span>
      </div>

      <hr style={styles.divider} />

      {/* Nav groups */}
      <nav style={styles.nav}>
        {SIDEBAR_GROUPS.map((group, gi) => (
          <div key={gi} style={styles.group}>
            {group.label && (
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
                  }}
                  onClick={() => navigate(item.section)}
                  aria-current={active ? "page" : undefined}
                >
                  <span style={{ color: active ? "var(--color-accent)" : "var(--color-text-secondary)" }}>
                    <NavIcon name={item.icon} />
                  </span>
                  <span style={{ color: active ? "var(--color-accent)" : "var(--color-text)" }}>
                    {item.label}
                  </span>
                </button>
              );
            })}
          </div>
        ))}
      </nav>

      {/* Footer */}
      <div style={styles.footer}>
        <hr style={styles.divider} />
        <div style={styles.serverStatus}>
          <span
            style={{
              ...styles.statusDot,
              background: state.serverOnline ? "var(--color-success)" : "var(--color-neutral)",
            }}
          />
          <span style={styles.statusText}>
            {state.serverOnline ? "Connected" : state.serverStarting ? "Starting…" : "Offline"}
          </span>
        </div>
        <div style={styles.modeButtons}>
          <button
            style={styles.modeBtn}
            onClick={switchToVoice}
            title="Switch to Voice mode"
            aria-label="Voice mode"
          >
            🎙 Voice
          </button>
          <button
            style={styles.modeBtn}
            onClick={switchToCanvas}
            title="Open Canvas overlay"
            aria-label="Canvas mode"
          >
            ✦ Canvas
          </button>
        </div>
      </div>
    </aside>
  );
}

const styles: Record<string, React.CSSProperties> = {
  sidebar: {
    width: "var(--sidebar-width)",
    minWidth: "var(--sidebar-width)",
    height: "100%",
    background: "var(--color-sidebar)",
    borderRight: "1px solid var(--color-border)",
    display: "flex",
    flexDirection: "column",
    overflow: "hidden",
    flexShrink: 0,
  },
  brand: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    padding: "16px 12px 12px",
    userSelect: "none",
  },
  brandLogo: {
    width: "28px",
    height: "28px",
    objectFit: "contain",
    flexShrink: 0,
  },
  brandName: {
    fontFamily: "var(--font-display)",
    fontWeight: 700,
    fontSize: "13px",
    color: "var(--color-text)",
    lineHeight: "1.3",
  },
  divider: {
    border: "none",
    borderTop: "1px solid var(--color-border)",
    margin: 0,
  },
  nav: {
    flex: 1,
    overflowY: "auto",
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
  },
  navItem: {
    width: "100%",
    display: "flex",
    alignItems: "center",
    gap: "8px",
    height: "var(--row-height-sm)",
    padding: "0 8px",
    borderRadius: "var(--radius-md)",
    background: "transparent",
    fontFamily: "var(--font-body)",
    fontSize: "var(--text-base)",
    fontWeight: "var(--weight-medium)" as unknown as number,
    cursor: "pointer",
    border: "none",
    textAlign: "left",
    transition: "background var(--transition-fast)",
    userSelect: "none",
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
  },
  serverStatus: {
    display: "flex",
    alignItems: "center",
    gap: "6px",
    padding: "8px 12px 4px",
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
  },
  modeButtons: {
    display: "flex",
    gap: "4px",
    padding: "0 8px",
  },
  modeBtn: {
    flex: 1,
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
  },
};
