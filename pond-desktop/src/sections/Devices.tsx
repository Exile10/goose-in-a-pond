import { useState, useEffect } from "react";
import { Table } from "@heroui/react";
import { api } from "../api/PondApiClient";
import type { Device } from "../api/types";

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

  if (loading) return <p style={hint}>Loading devices…</p>;
  if (error)   return <p style={{ ...hint, color: "var(--color-destructive)" }}>{error}</p>;
  if (!devices.length) return <p style={hint}>No devices registered yet.</p>;

  return (
    <div style={{ maxWidth: "var(--content-max-width)" }}>
      <Table>
        <Table.ScrollContainer>
          <Table.Content aria-label="Devices">
            <Table.Header>
              <Table.Column>Name</Table.Column>
              <Table.Column>Room</Table.Column>
              <Table.Column>Status</Table.Column>
              <Table.Column>Last seen</Table.Column>
            </Table.Header>
            <Table.Body>
              {devices.map((d) => (
                <Table.Row key={d.id}>
                  <Table.Cell>{d.name}</Table.Cell>
                  <Table.Cell>
                    <span style={{ color: "var(--color-text-secondary)" }}>{d.room ?? "—"}</span>
                  </Table.Cell>
                  <Table.Cell>
                    <span style={{ display: "flex", alignItems: "center", gap: "6px" }}>
                      <span style={{
                        width: "7px", height: "7px", borderRadius: "50%", flexShrink: 0,
                        background: d.is_online ? "var(--color-success)" : "var(--color-neutral)",
                      }} />
                      {d.is_online ? "Online" : "Offline"}
                    </span>
                  </Table.Cell>
                  <Table.Cell>
                    <span style={{ color: "var(--color-text-tertiary)", fontFamily: "var(--font-mono)", fontSize: "var(--text-sm)" }}>
                      {d.last_seen ? new Date(d.last_seen).toLocaleString() : "—"}
                    </span>
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table.Content>
        </Table.ScrollContainer>
      </Table>
    </div>
  );
}

const hint: React.CSSProperties = { color: "var(--color-text-tertiary)", fontSize: "var(--text-sm)", margin: 0 };
