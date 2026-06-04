import { HubIco } from "../../primitives/HubIco";
import { HP_PATHS } from "../../primitives/icons";

interface GooseAvatarProps {
  size?: number;
}

export function GooseAvatar({ size = 30 }: GooseAvatarProps) {
  return (
    <span
      className="ch-avatar"
      style={{ width: size, height: size }}
      aria-hidden="true"
    >
      <HubIco
        d={HP_PATHS.goose}
        size={size * 0.58}
        color="#fff"
        sw={2}
      />
    </span>
  );
}
