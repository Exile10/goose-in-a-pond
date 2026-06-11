const WIDTHS = [72, 88, 60, 78, 84];

export function SkeletonList({ rows = 3 }: { rows?: number }) {
  return (
    <div className="skeleton-list" aria-busy="true" aria-label="Loading">
      {Array.from({ length: rows }).map((_, i) => (
        <div key={i} className="skeleton-row">
          <div className="skeleton-line" style={{ width: `${WIDTHS[i % WIDTHS.length]}%` }} />
          <div className="skeleton-line skeleton-line--short" style={{ width: `${WIDTHS[(i + 2) % WIDTHS.length] * 0.6}%` }} />
        </div>
      ))}
    </div>
  );
}
