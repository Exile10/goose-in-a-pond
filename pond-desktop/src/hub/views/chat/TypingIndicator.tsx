import { GooseAvatar } from "./GooseAvatar";

export function TypingIndicator() {
  return (
    <div className="ch-row ch-row--goose" role="status" aria-label="Goose is typing">
      <GooseAvatar />
      <div className="ch-bubble ch-bubble--goose ch-typing">
        <span />
        <span />
        <span />
      </div>
    </div>
  );
}
