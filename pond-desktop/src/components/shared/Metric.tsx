export function Metric({ label, value, trend }: { label: string; value: string; trend?: string }) {
  return (
    <div className="metric">
      <div className="metric__label">{label}</div>
      <div className={`metric__value${trend ? ` metric__value--${trend}` : ""}`}>{value}</div>
    </div>
  );
}
