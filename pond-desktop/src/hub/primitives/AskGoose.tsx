import { useState, useEffect } from "react";
import { HubIco, micEl } from "./HubIco";
import { HP_PATHS } from "./icons";
import { HOME } from "../data/mockHome";

interface AskGooseProps {
  go?: (route: string) => void;
}

export function AskGoose({ go }: AskGooseProps) {
  const [i, setI] = useState(0);

  useEffect(() => {
    const t = setInterval(() => setI((v) => (v + 1) % HOME.gooseSuggestions.length), 3400);
    return () => clearInterval(t);
  }, []);

  return (
    <button className="askgoose" onClick={() => go?.("chat")}>
      <span className="askgoose__mic">
        <span className="askgoose__pulse" />
        <HubIco d={micEl} size={20} color="#fff" />
      </span>
      <span className="askgoose__text">
        <span className="askgoose__hint">Ask Goose</span>
        <span className="askgoose__say">"{HOME.gooseSuggestions[i]}"</span>
      </span>
      <span className="askgoose__kb">
        <HubIco d={HP_PATHS.keyboard} size={18} color="rgba(255,255,255,.85)" />
      </span>
    </button>
  );
}
