import { useState, useEffect, useRef, useCallback } from "react";
import { api } from "./api";
import { SettingsProvider } from "./context/SettingsContext";
import Dashboard from "./pages/Dashboard";
import Devices from "./pages/Devices";
import Status from "./pages/Status";
import Settings from "./pages/Settings";
import Activity from "./pages/Activity";
import Schedules from "./pages/Schedules";
import Models from "./pages/Models";
import Agent from "./pages/Agent";
import Prompts from "./pages/Prompts";
import FaceEnrollment from "./pages/FaceEnrollment";
import Onboarding from "./pages/Onboarding";
import VoiceOrb from "./components/VoiceOrb";
import logo from "./assets/logo.png";
import "./dashboard.css";

type Page = "chat" | "devices" | "activity" | "status" | "settings" | "schedules" | "models" | "agent" | "prompts" | "faces";

function getInitials(name: string): string {
    return name.split(' ').map(w => w[0]).filter(Boolean).slice(0, 2).join('').toUpperCase() || '?'
}

const NAV_ITEMS: { page: Page; label: string; icon: React.ReactNode }[] = [
    {
        page: "chat",
        label: "Chat",
        icon: (
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
            </svg>
        ),
    },
    {
        page: "devices",
        label: "Devices",
        icon: (
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <rect x="2" y="3" width="20" height="14" rx="2" />
                <line x1="8" y1="21" x2="16" y2="21" />
                <line x1="12" y1="17" x2="12" y2="21" />
            </svg>
        ),
    },
    {
        page: "schedules",
        label: "Schedules",
        icon: (
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <rect x="3" y="4" width="18" height="18" rx="2" ry="2" />
                <line x1="16" y1="2" x2="16" y2="6" />
                <line x1="8" y1="2" x2="8" y2="6" />
                <line x1="3" y1="10" x2="21" y2="10" />
            </svg>
        ),
    },
    {
        page: "activity",
        label: "Activity",
        icon: (
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <polyline points="22 12 18 12 15 21 9 3 6 12 2 12" />
            </svg>
        ),
    },
    {
        page: "status",
        label: "System Status",
        icon: (
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <circle cx="12" cy="12" r="10" />
                <line x1="12" y1="8" x2="12" y2="12" />
                <line x1="12" y1="16" x2="12.01" y2="16" />
            </svg>
        ),
    },
    {
        page: "models",
        label: "Models",
        icon: (
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <path d="M21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16z" />
            </svg>
        ),
    },
    {
        page: "settings",
        label: "Settings",
        icon: (
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <circle cx="12" cy="12" r="3" />
                <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
            </svg>
        ),
    },
    {
        page: "agent",
        label: "Agent",
        icon: (
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <path d="M12 2a5 5 0 0 1 5 5v3H7V7a5 5 0 0 1 5-5z" />
                <rect x="3" y="10" width="18" height="11" rx="2" />
                <circle cx="8.5" cy="15.5" r="1.5" />
                <circle cx="15.5" cy="15.5" r="1.5" />
            </svg>
        ),
    },
    {
        page: "prompts",
        label: "Prompts",
        icon: (
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <line x1="8" y1="6" x2="21" y2="6" />
                <line x1="8" y1="12" x2="21" y2="12" />
                <line x1="8" y1="18" x2="21" y2="18" />
                <line x1="3" y1="6" x2="3.01" y2="6" />
                <line x1="3" y1="12" x2="3.01" y2="12" />
                <line x1="3" y1="18" x2="3.01" y2="18" />
            </svg>
        ),
    },
    {
        page: "faces",
        label: "Faces",
        icon: (
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <circle cx="12" cy="8" r="4" />
                <path d="M4 21a8 8 0 0 1 16 0" />
            </svg>
        ),
    },
];

function App() {
    const [page, setPage] = useState<Page>("chat");

    // Apply saved theme preference on mount
    useEffect(() => {
        const saved = localStorage.getItem("pond_theme") ?? "system";
        if (saved === "system") {
            delete document.documentElement.dataset.theme;
        } else {
            document.documentElement.dataset.theme = saved;
        }
    }, []);
    const [token, setToken] = useState<string>(
        () => localStorage.getItem("pond_session_token") ?? ""
    );
    const [displayName, setDisplayName] = useState<string>(
        () => localStorage.getItem("pond_display_name") ?? ""
    );
    // null = still checking, false = show onboarding, true = show dashboard
    const [onboarded, setOnboarded] = useState<boolean | null>(
        () => localStorage.getItem("pond_session_token") ? true : null
    );
    const [menuOpen, setMenuOpen] = useState(false);
    const menuRef = useRef<HTMLDivElement>(null);

    // On mount (or when no local token), probe the backend.
    // If the pond is already onboarded, auto-handshake so the user goes
    // straight to the dashboard regardless of which hostname they use.
    const autoConnect = useCallback(async () => {
        try {
            const status = await api.onboardingStatus();
            if (!status.onboarded) {
                setOnboarded(false);
                return;
            }
            // Pond is onboarded — get/create a client ID and handshake
            let clientId = localStorage.getItem("pond_client_id") ?? "";
            if (!clientId) {
                clientId = typeof crypto.randomUUID === "function"
                    ? crypto.randomUUID()
                    : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
                localStorage.setItem("pond_client_id", clientId);
            }
            const res = await api.handshake({ client_id: clientId, client_type: "web", client_version: "1.0.0" });
            if (res.accepted && res.session_token) {
                localStorage.setItem("pond_session_token", res.session_token);
                setToken(res.session_token);
                setOnboarded(true);
            } else {
                setOnboarded(false);
            }
        } catch {
            // Backend unreachable — fall through to onboarding (which will also fail and show an error)
            setOnboarded(false);
        }
    }, []);

    useEffect(() => {
        if (onboarded === null) {
            autoConnect();
        }
    }, [onboarded, autoConnect]);

    useEffect(() => {
        function handleClickOutside(e: MouseEvent) {
            if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
                setMenuOpen(false);
            }
        }
        document.addEventListener("mousedown", handleClickOutside);
        return () => document.removeEventListener("mousedown", handleClickOutside);
    }, []);

    function handleSignOut() {
        localStorage.removeItem("pond_session_token");
        localStorage.removeItem("pond_display_name");
        localStorage.removeItem("pond_client_id");
        setToken("");
        setDisplayName("");
        setMenuOpen(false);
    }

    if (onboarded === null) {
        // Still checking with the backend — show a minimal splash
        return (
            <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: '100vh', background: 'var(--bg-base, #0f1117)' }}>
                <span style={{ color: 'rgba(255,255,255,0.35)', fontSize: '0.9rem' }}>Connecting…</span>
            </div>
        );
    }

    if (!onboarded) {
        return (
            <Onboarding onComplete={(t, name) => {
                setToken(t);
                setDisplayName(name);
                setOnboarded(true);
                setPage("chat");
            }} />
        );
    }

    return (
        <SettingsProvider token={token}>
        <div className="db-shell">
            <aside className="db-sidebar">
                {/* Logo */}
                <div className="db-sidebar-logo">
                    <img src={logo} alt="Goose In A Pond" className="db-sidebar-logo-img" />
                </div>

                {/* Nav links */}
                <nav className="db-sidebar-nav">
                    {NAV_ITEMS.map(item => (
                        <button
                            key={item.page}
                            className={`db-sidebar-link ${page === item.page ? "active" : ""}`}
                            onClick={() => setPage(item.page)}
                        >
                            <span className="db-sidebar-link-icon">{item.icon}</span>
                            <span className="db-sidebar-link-label">{item.label}</span>
                        </button>
                    ))}
                </nav>

                {/* Avatar + sign out at bottom */}
                <div className="db-sidebar-footer" ref={menuRef}>
                    <button
                        className="db-sidebar-profile"
                        onClick={() => setMenuOpen(o => !o)}
                    >
                        <span className="db-sidebar-avatar">{getInitials(displayName)}</span>
                        <span className="db-sidebar-profile-name">{displayName || "Profile"}</span>
                    </button>

                    {menuOpen && (
                        <div className="db-sidebar-menu">
                            <button className="db-sidebar-menu-item db-sidebar-menu-danger" onClick={handleSignOut}>
                                Sign out
                            </button>
                        </div>
                    )}
                </div>
            </aside>

            <main className="db-main">
                {page === "chat"      && <Dashboard token={token} />}
                {page === "devices"   && <Devices token={token} />}
                {page === "schedules" && <Schedules token={token} />}
                {page === "activity"  && <Activity token={token} />}
                {page === "status"    && <Status token={token} />}
                {page === "models"    && <Models token={token} />}
                {page === "settings"  && <Settings token={token} />}
                {page === "agent"     && <Agent token={token} />}
                {page === "prompts"   && <Prompts token={token} />}
                {page === "faces"     && <FaceEnrollment token={token} />}
            </main>

            {/* Voice orb — always accessible regardless of page */}
            <VoiceOrb token={token} />
        </div>
        </SettingsProvider>
    );
}

export default App;
