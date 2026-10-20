import { useCallback, useEffect, useRef, useState } from "react";
import { DetailShell } from "./DetailShell";
import { Card, Row, Toggle } from "./controls";
import { api } from "../../../api/PondApiClient";
import type { Settings } from "../../../api/types";
import { ConnectionsProvider } from "../../../network/ConnectionsProvider";
import { BluetoothStatus, EthernetPanel, Message, PairingStatus } from "../../../network/LinkPanels";
import { WifiPanel } from "../../../network/WifiPanel";

type Switch = "ble_setup_enabled" | "spoken_pairing_codes";

export function NetworkDetail({ go }: { go: (route: string) => void }) {
  const [settings, setSettings] = useState<Partial<Settings>>({});
  const [flash, setFlash] = useState<{ text: string; ok: boolean } | null>(null);
  // Counts saved switches, so the Bluetooth status is read again once the radio has followed one.
  const [saved, setSaved] = useState(0);
  const flashTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const showFlash = useCallback((text: string, ok = true) => {
    if (flashTimer.current) clearTimeout(flashTimer.current);
    setFlash({ text, ok });
    flashTimer.current = setTimeout(() => setFlash(null), 3000);
  }, []);

  useEffect(() => {
    let alive = true;
    api.getSettings().then(
      (s) => alive && setSettings(s),
      (e) => console.warn("[NetworkDetail] API offline, using defaults:", e),
    );
    return () => {
      alive = false;
      if (flashTimer.current) clearTimeout(flashTimer.current);
    };
  }, []);

  async function handleToggle(field: Switch, value: boolean) {
    setSettings((prev) => ({ ...prev, [field]: value }));
    try {
      const updated = await api.updateSettings({ [field]: value });
      setSettings(updated);
      setSaved((n) => n + 1);
      showFlash("Setting saved.");
    } catch {
      setSettings((prev) => ({ ...prev, [field]: !value }));
      showFlash("That switch was not saved. Try again.", false);
    }
  }

  return (
    <DetailShell
      title="Connections"
      subtitle="How the pond reaches your network and your phone"
      accent="var(--color-info-fg)"
      onBack={() => go("settings")}
    >
      {flash && <Message tone={flash.ok ? "ok" : "problem"} text={flash.text} />}

      <ConnectionsProvider refreshKey={saved}>
        <Card title="Wi-Fi">
          <WifiPanel variant="hub" />
        </Card>

        <Card title="Ethernet">
          <EthernetPanel variant="hub" />
        </Card>

        <Card title="Bluetooth">
          <BluetoothStatus variant="hub" />
          <Row
            label="Set up over Bluetooth"
            sub="A phone nearby can find the pond while it is unclaimed or offline"
            control={
              <Toggle
                key={`ble-setup-${settings.ble_setup_enabled ?? true}`}
                on={settings.ble_setup_enabled ?? true}
                onChange={(v) => void handleToggle("ble_setup_enabled", v)}
                label="Set up over Bluetooth"
              />
            }
          />
        </Card>

        <Card title="Pairing">
          <PairingStatus variant="hub" />
          <Row
            label="Say pairing codes aloud"
            sub="A phone at home can ask; only someone in the room hears it"
            control={
              <Toggle
                key={`spoken-codes-${settings.spoken_pairing_codes ?? true}`}
                on={settings.spoken_pairing_codes ?? true}
                onChange={(v) => void handleToggle("spoken_pairing_codes", v)}
                label="Say pairing codes aloud"
              />
            }
          />
        </Card>
      </ConnectionsProvider>
    </DetailShell>
  );
}
