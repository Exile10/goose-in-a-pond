import { useState } from "react";
import Dashboard from "./pages/Dashboard";
import Settings from "./pages/Settings";
import Onboarding from "./pages/Onboarding";

function App() {
    const [page, setPage] = useState<"dashboard" | "settings" | "onboarding">("dashboard");

    return (
        <div>
            <nav>
                <button onClick={() => setPage("dashboard")}>Dashboard</button>
                <button onClick={() => setPage("settings")}>Settings</button>
                <button onClick={() => setPage("onboarding")}>Onboarding</button>
            </nav>

            <main>
                {page === "dashboard" && <Dashboard />}
                {page === "settings" && <Settings />}
                {page === "onboarding" && <Onboarding />}
            </main>
        </div>
    );
}

export default App;
