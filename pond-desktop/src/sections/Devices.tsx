import { useState, useEffect } from "react";
import { Card, CardContent, Button, Chip } from "@heroui/react";
import { Monitor, Cpu, Activity, Power, Settings, Plus } from "lucide-react";
import { api } from "../api/PondApiClient";
import { PageHeader } from "../components/shared";
import type { Device } from "../api/types";

function deviceIcon(kind: string | undefined): React.ReactNode {
  switch (kind) {
    case "host":   return <Cpu size={18} />;
    case "sensor": return <Activity size={18} />;
    default:       return <Monitor size={18} />;
  }
}

function iconClass(kind: string | undefined, isOnline: boolean): string {
  if (!isOnline) return "device-card__icon";
  switch (kind) {
    case "host":   return "device-card__icon device-card__icon--host";
    case "sensor": return "device-card__icon device-card__icon--sensor";
    default:       return "device-card__icon device-card__icon--edge";
  }
}

function timeSince(iso: string | null | undefined): string {
  if (!iso) return "\u2014";
  const diff = Date.now() - new Date(iso).getTime();
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins} min ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)} days ago`;
}

export function Devices() {
  const [devices, setDevices] = useState<Device[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.listDevices()
      .then(setDevices)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  return (
    <div className="screen screen--devices">
      <PageHeader
        title="Devices"
        action={
          <Button color="secondary" radius="md" startContent={<Plus size={14} />}>
            Register device
          </Button>
        }
      />

      {loading && <p className="muted-12">Loading devices...</p>}
      {error && <p className="muted-12 text-error">{error}</p>}

      {!loading && !error && devices.length === 0 && (
        <div className="empty-state">
          <Monitor size={32} />
          <span>No devices registered yet.</span>
        </div>
      )}

      {devices.length > 0 && (
        <div className="devices-grid">
          {devices.map((d) => (
            <Card
              key={d.id}
              className={`card device-card${!d.is_online ? " device-card--offline" : ""}`}
            >
              <CardContent>
                <div className="device-card__head">
                  <div className={iconClass(d.device_type, d.is_online)}>
                    {deviceIcon(d.device_type)}
                  </div>
                  <Chip
                    size="sm"
                    variant="flat"
                    color={d.is_online ? "success" : "default"}
                    startContent={<span className={`status-dot status-dot--${d.is_online ? "online" : "offline"}`} />}
                  >
                    {d.is_online ? "online" : "offline"}
                  </Chip>
                </div>

                <div className="device-card__name">{d.name}</div>

                <code className="device-card__ip">
                  {d.metadata?.ip ?? "\u2014"}
                </code>

                <div className="device-card__meta">
                  <span className="muted-12">{d.device_type ?? "edge"}</span>
                  <span className="muted-12">&middot;</span>
                  <span className="muted-12">{timeSince(d.last_seen)}</span>
                </div>

                <div className="card__divider card__divider--device" />

                <div className="device-card__actions">
                  <Button size="sm" variant="light" startContent={<Power size={13} />}>
                    {d.is_online ? "Restart" : "Wake"}
                  </Button>
                  <Button size="sm" variant="light" startContent={<Settings size={13} />}>
                    Configure
                  </Button>
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      )}
    </div>
  );
}
