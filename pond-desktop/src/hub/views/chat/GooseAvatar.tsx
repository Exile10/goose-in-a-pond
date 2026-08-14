import { Goose } from "../../../components/Goose";

interface GooseAvatarProps {
  size?: number;
  /** Drives the bird's animation — lets a caller show it working. */
  state?: "idle" | "working" | "listening";
}

/**
 * The goose, small, in a badge.
 *
 * Kept as its own component because Canvas and the hub chat both place a goose
 * beside a message and expect a fixed round mark. It now draws the shared
 * `<Goose />` rather than the old 24px line glyph, so all three screens moved
 * to the new artwork at once.
 */
export function GooseAvatar({ size = 30, state = "idle" }: GooseAvatarProps) {
  return (
    <span
      className="ch-avatar"
      style={{ "--av-size": `${size}px` } as React.CSSProperties}
      aria-hidden="true"
    >
      <Goose state={state} size={size * 0.78} water={false} />
    </span>
  );
}
