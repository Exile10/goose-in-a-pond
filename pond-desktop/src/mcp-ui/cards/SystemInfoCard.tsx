import { Cpu, HardDrive, Monitor, Loader } from "lucide-react";
import { registerMcpCard, type McpCardProps } from "../registry";

interface MetricEntry {
  label: string;
  value: string;
  icon: "monitor" | "cpu" | "ram" | "disk";
}

function metricIcon(type: MetricEntry["icon"]) {
  switch (type) {
    case "monitor": return <Monitor size={13} style={{ color: "#8A8A8A" }} />;
    case "cpu": return <Cpu size={13} style={{ color: "#8A8A8A" }} />;
    case "ram": return <HardDrive size={13} style={{ color: "#8A8A8A" }} />;
    case "disk": return <HardDrive size={13} style={{ color: "#8A8A8A" }} />;
    default: return <Monitor size={13} style={{ color: "#8A8A8A" }} />;
  }
}

function SystemInfoCard({ data, variant }: McpCardProps) {
  const isCompact = variant === "compact";
  const platform = data.platform as string | undefined;
  const arch = data.arch as string | undefined;
  const memoryTotal = data.memory_total as string | undefined;
  const cpu = data.cpu as string | undefined;
  const hostname = data.hostname as string | undefined;
  const osVersion = data.os_version as string | undefined;

  const hasData = platform || arch || memoryTotal || cpu;

  if (!hasData) {
    return (
      <div className="ui-card ui-system">
        <div style={{ display: "flex", alignItems: "center", gap: 8, padding: "8px 0" }}>
          <Loader size={16} style={{ animation: "spin 1.5s linear infinite", color: "#8C4BFF" }} />
          <span style={{ fontSize: 13, color: "#8A8A8A" }}>Fetching system info...</span>
        </div>
        <style>{`@keyframes spin { to { transform: rotate(360deg); } }`}</style>
      </div>
    );
  }

  const metrics: MetricEntry[] = [
    ...(platform ? [{ label: "Platform", value: platform, icon: "monitor" as const }] : []),
    ...(arch ? [{ label: "Architecture", value: arch, icon: "cpu" as const }] : []),
    ...(memoryTotal ? [{ label: "Memory", value: memoryTotal, icon: "ram" as const }] : []),
    ...(cpu ? [{ label: "CPU", value: cpu, icon: "cpu" as const }] : []),
  ];

  const visibleMetrics = isCompact ? metrics.slice(0, 2) : metrics;

  return (
    <div className="ui-card ui-system">
      <div className="ui-system__header">
        <Monitor size={14} style={{ color: "#5F5F5F" }} />
        {hostname && <span className="ui-system__hostname">{hostname}</span>}
        {osVersion && !isCompact && (
          <span className="ui-system__os-version">{osVersion}</span>
        )}
      </div>

      <div className={`ui-system__grid ${isCompact ? "ui-system__grid--compact" : ""}`}>
        {visibleMetrics.map((m) => (
          <div key={m.label} className="ui-system__metric">
            <div className="ui-system__metric-header">
              {metricIcon(m.icon)}
              <span className="ui-system__metric-label">{m.label}</span>
            </div>
            <span className="ui-system__metric-value">{m.value}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

registerMcpCard({
  key: "system",
  label: "System Info",
  icon: "Monitor",
  toolPattern: /system_info|get_system_info/,
  component: SystemInfoCard,
  mockTool: "giap-system__get_system_info",
  mockData: {
    platform: "macOS",
    arch: "arm64",
    memory_total: "16 GB",
    cpu: "Apple M2 Pro",
    hostname: "jerry-macbook.local",
    os_version: "macOS 15.5",
  },
});
