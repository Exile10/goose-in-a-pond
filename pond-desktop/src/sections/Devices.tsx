import { useState, useEffect } from "react";
import { Card, CardContent, Button, Chip } from "@heroui/react";
import { Monitor, Cpu, Activity, Power, Settings } from "lucide-react";
import { api } from "../api/PondApiClient";
import type { Device } from "../api/types";

function deviceIcon(kind: string | undefined): React.ReactNode {
  switch (kind) {
    case "host":   return <Cpu size={18} />;
    case "sensor": return <Activity size={18} />;
    default:       return <Monitor size={18} />;
  }
}

function iconClass(kind: string | undefined): string {
  switch (kind) {
    case "host":   return "device-card__icon device-card__icon--host";
    case "sensor": return "device-card__icon device-card__icon--sensor";
    default:       return "device-card__icon device-card__icon--edge";
  }
}

export function Devices() {
  const [devices, setDevices] = useState<Device[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError]     = useState<string | null>(null);

  useEffect(() => {
    api.listDevices()
      .then(setDevices)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  return (
    <div className="screen">
      <div className="page-header">
        <h1 className="page-header__title">Devices</h1>
      </div>

      {loading && <p className="muted-12">Loading devices\u2026</p>}
      {error && <p className="muted-12" style={{ color: "var(--color-destructive)" }}>{error}</p>}

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
              shadow="none"
              className={`giap-card ${!d.is_online ? "device-card--offline" : ""}`}
            >
              <CardContent>
                <div className="device-card__head">
                  <div className={iconClass(d.device_type)}>
                    {deviceIcon(d.device_type)}
                  </div>
                  <Chip
                    size="sm"
                    variant="flat"
                    color={d.is_online ? "success" : "default"}
                  >
                    <span className={`giap-status-dot giap-status-dot--${d.is_online ? "online" : "offline"}`} />
                    {d.is_online ? "Online" : "Offline"}
                  </Chip>
                </div>

                <div className="device-card__name">{d.name}</div>

                {d.room && (
                  <code style={{ fontSize: 11, color: "var(--grey-500)", display: "block", marginBottom: 6 }}>
                    {d.room}
                  </code>
                )}

                <div className="device-card__meta">
                  <Chip size="sm" variant="flat" color="default">{d.device_type ?? "edge"}</Chip>
                  <span className="muted-12">
                    {d.last_seen ? new Date(d.last_seen).toLocaleString() : "\u2014"}
                  </span>
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      )}
    </div>
  );
}
