import { InkProvider } from "@jarida/ink/react";

import { useAppState, useAppDispatch } from "./state/AppContext";
import { GuiMode } from "./modes/GuiMode";
import { VoiceMode } from "./modes/voice";
import { OnboardingWizard } from "./components/onboarding";
import { api } from "./api/PondApiClient";
import { useTheme } from "./hub/state/themeStore";

/**
 * Ink, in `context` mode: it hands components the theme and writes nothing.
 *
 * The custom properties are already on `<html>` — `themeStore` puts them there
 * on module load, before React renders, which is what keeps the first paint
 * from flashing. Letting the provider write them too would mean two writers of
 * the same eighty properties, and the second one arriving a frame late.
 *
 * It is given the exact theme object the store built, so what the DOM says and
 * what a component reads can never be two different themes.
 */
function InkScope({ children }: { children: React.ReactNode }) {
  const { ink } = useTheme();
  return (
    <InkProvider mode="context" theme={ink}>
      {children}
    </InkProvider>
  );
}

export function App() {
  const { mode, needsOnboarding } = useAppState();
  const dispatch = useAppDispatch();

  // Show onboarding wizard when the backend reports the device is not yet onboarded.
  if (needsOnboarding) {
    return (
      <InkScope>
        <OnboardingWizard
        onComplete={async () => {
          try {
            await api.completeOnboarding();
          } catch (err) {
            console.warn("completeOnboarding failed (non-fatal):", err);
          }
            dispatch({ type: "SET_NEEDS_ONBOARDING", payload: false });
          }}
        />
      </InkScope>
    );
  }

  return <InkScope>{mode === "voice" ? <VoiceMode /> : <GuiMode />}</InkScope>;
}
