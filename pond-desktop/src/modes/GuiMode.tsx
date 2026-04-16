import { Sidebar } from "../components/Sidebar";
import { useAppState } from "../state/AppContext";
import { getDesktopSectionLabel } from "../desktopState";
import type { GuiSection } from "../desktopState";

// Lazy section imports
import { Dashboard } from "../sections/Dashboard";
import { Chat } from "../sections/Chat";
import { Devices } from "../sections/Devices";
import { Schedules } from "../sections/Schedules";
import { Memory } from "../sections/Memory";
import { Skills } from "../sections/Skills";
import { Models } from "../sections/Models";
import { Prompts } from "../sections/Prompts";
import { Settings } from "../sections/Settings";
import { Agent } from "../sections/Agent";

function SectionContent({ section }: { section: GuiSection }) {
  switch (section) {
    case "dashboard": return <Dashboard />;
    case "chat":      return <Chat />;
    case "devices":   return <Devices />;
    case "schedules": return <Schedules />;
    case "memory":    return <Memory />;
    case "skills":    return <Skills />;
    case "models":    return <Models />;
    case "prompts":   return <Prompts />;
    case "settings":  return <Settings />;
    case "agent":     return <Agent />;
  }
}

export function GuiMode() {
  const state = useAppState();

  return (
    <div style={styles.shell}>
      <Sidebar />
      <div style={styles.content}>
        {/* Toolbar */}
        <header style={styles.toolbar}>
          <h2 style={styles.sectionTitle}>
            {getDesktopSectionLabel(state.section)}
          </h2>
        </header>
        {/* Scrollable section area */}
        <main style={styles.main}>
          <SectionContent section={state.section} />
        </main>
      </div>
    </div>
  );
}

const styles: Record<string, React.CSSProperties> = {
  shell: {
    display: "flex",
    flexDirection: "row",
    height: "100%",
    width: "100%",
    overflow: "hidden",
    background: "var(--color-bg)",
  },
  content: {
    flex: 1,
    display: "flex",
    flexDirection: "column",
    overflow: "hidden",
    background: "var(--color-content)",
  },
  toolbar: {
    height: "var(--toolbar-height)",
    borderBottom: "1px solid var(--color-border)",
    display: "flex",
    alignItems: "center",
    padding: "0 var(--space-6)",
    flexShrink: 0,
    background: "var(--color-bg)",
  },
  sectionTitle: {
    fontFamily: "var(--font-display)",
    fontWeight: 700,
    fontSize: "var(--text-md)",
    color: "var(--color-text)",
    margin: 0,
  },
  main: {
    flex: 1,
    overflowY: "auto",
    padding: "var(--space-6)",
  },
};
