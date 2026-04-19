import { useAppState } from "./state/AppContext";
import { GuiMode } from "./modes/GuiMode";
import { VoiceMode } from "./modes/VoiceMode";

export function App() {
  const { mode } = useAppState();

  // Canvas mode uses a separate Tauri window (canvas.html / canvas-main.tsx).
  // The main window shows GUI or Voice.
  if (mode === "voice") return <VoiceMode />;
  return <GuiMode />;
}
