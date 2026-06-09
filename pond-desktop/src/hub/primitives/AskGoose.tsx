import { useState, useEffect } from "react";
import { HubIco, micEl } from "./HubIco";
import { HP_PATHS } from "./icons";
import { useHomeData } from "../state/hubDataStore";

interface AskGooseProps {
  go?: (route: string) => void;
}

export function AskGoose({ go }: AskGooseProps) {
  const [i, setI] = useState(0);
  const { gooseSuggestions } = useHomeData();

  useEffect(() => {
    const t = setInterval(() => setI((v) => (v + 1) % gooseSuggestions.length), 3400);
    return () => clearInterval(t);
  }, [gooseSuggestions.length]);

  return (
    <button className="askgoose" onClick={() => go?.("chat")}>
      <span className="askgoose__mic">
        <span className="askgoose__pulse" />
        <HubIco d={micEl} size={20} color="#fff" />
      </span>
      <span className="askgoose__text">
        <span className="askgoose__hint">Ask Goose</span>
        <span className="askgoose__say">"{gooseSuggestions[i]}"</span>
      </span>
      <span className="askgoose__kb">
        <HubIco d={HP_PATHS.keyboard} size={18} color="rgba(255,255,255,.85)" />
      </span>
    </button>
  );
}
