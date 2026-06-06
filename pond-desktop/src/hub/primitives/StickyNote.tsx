import { useEffect, useRef, useState } from "react";
import { HubIco } from "./HubIco";
import { HP_PATHS } from "./icons";

const STORAGE_KEY = "goosehub_sticky";

function readStored(): string {
  try {
    return localStorage.getItem(STORAGE_KEY) ?? "";
  } catch {
    return "";
  }
}

function writeStored(value: string): void {
  try {
    if (value) {
      localStorage.setItem(STORAGE_KEY, value);
    } else {
      localStorage.removeItem(STORAGE_KEY);
    }
  } catch {
    // ignore
  }
}

export function StickyNote() {
  const [content, setContent] = useState<string>(readStored);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // Hydrate from localStorage on mount (handles cross-tab sync)
  useEffect(() => {
    setContent(readStored());
  }, []);

  function startEdit() {
    setDraft(content);
    setEditing(true);
    // Focus textarea after render
    requestAnimationFrame(() => textareaRef.current?.focus());
  }

  function save() {
    const trimmed = draft.trim();
    setContent(trimmed);
    writeStored(trimmed);
    setEditing(false);
  }

  function cancel() {
    setEditing(false);
    setDraft("");
  }

  function del() {
    setContent("");
    writeStored("");
    setEditing(false);
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Escape") cancel();
    // Ctrl+Enter or Cmd+Enter to save
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) save();
  }

  if (editing) {
    return (
      <div className="sticky sticky--editing">
        <textarea
          ref={textareaRef}
          className="sticky__textarea"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder="Write a note…"
          rows={5}
        />
        <div className="sticky__actions">
          <button className="sticky__save" onClick={save}>
            <HubIco d={HP_PATHS.check} size={13} color="#fff" sw={2.5} /> Save
          </button>
          <button className="sticky__cancel" onClick={cancel}>
            Cancel
          </button>
        </div>
      </div>
    );
  }

  if (content) {
    return (
      <div className="sticky sticky--filled">
        <div className="sticky__body">{content}</div>
        <div className="sticky__actions sticky__actions--filled">
          <button className="sticky__edit" onClick={startEdit} aria-label="Edit sticky note">
            <HubIco d={HP_PATHS.sliders} size={12} color="var(--pp)" /> Edit
          </button>
          <button className="sticky__del" onClick={del} aria-label="Delete sticky note">
            <HubIco d="M18 6L6 18M6 6l12 12" size={12} color="var(--tc-dim)" /> Delete
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="sticky">
      <div className="sticky__body">Try, "Goose make a new sticky note"</div>
      <button className="sticky__cta" onClick={startEdit}>+ Create sticky</button>
    </div>
  );
}
