import React from "react";

interface HubIcoProps {
  /** Either a plain SVG path string (d attribute) or a ReactNode for multi-path icons */
  d: string | React.ReactNode;
  size?: number;
  color?: string;
  fill?: string;
  sw?: number;
  className?: string;
}

/** Thin SVG icon wrapper (the design's HIco). */
export function HubIco({
  d,
  size = 20,
  color = "currentColor",
  fill = "none",
  sw = 1.9,
  className,
}: HubIcoProps): React.ReactElement {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill={fill}
      stroke={color}
      strokeWidth={sw}
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ flexShrink: 0, display: "block" }}
      className={className}
    >
      {typeof d === "string" ? <path d={d} /> : d}
    </svg>
  );
}
