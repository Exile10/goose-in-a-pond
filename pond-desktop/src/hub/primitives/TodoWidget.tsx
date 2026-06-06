import { useEffect, useRef, useState } from "react";
import { HubIco } from "./HubIco";
import { HP_PATHS } from "./icons";

const STORAGE_KEY = "goosehub_todos";

interface TodoItem {
  id: string;
  text: string;
  done: boolean;
}

function readStored(): TodoItem[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) return JSON.parse(raw) as TodoItem[];
  } catch {
    // ignore
  }
  // Seed with mock defaults on first load
  return [
    { id: "seed-1", text: "Water the plants",     done: true },
    { id: "seed-2", text: "Call plumber re: leak", done: false },
    { id: "seed-3", text: "Order coffee beans",   done: false },
  ];
}

function writeStored(items: TodoItem[]): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(items));
  } catch {
    // ignore
  }
}

let idCounter = Date.now();
function nextId(): string {
  return String(++idCounter);
}

export function TodoWidget() {
  const [items, setItems]       = useState<TodoItem[]>(readStored);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editText, setEditText]   = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  // Hydrate from localStorage on mount
  useEffect(() => {
    setItems(readStored());
  }, []);

  function persist(next: TodoItem[]) {
    setItems(next);
    writeStored(next);
  }

  function toggle(id: string) {
    persist(items.map((it) => it.id === id ? { ...it, done: !it.done } : it));
  }

  function startEdit(id: string, text: string) {
    setEditingId(id);
    setEditText(text);
    requestAnimationFrame(() => inputRef.current?.focus());
  }

  function commitEdit(id: string) {
    const trimmed = editText.trim();
    if (trimmed) {
      persist(items.map((it) => it.id === id ? { ...it, text: trimmed } : it));
    } else {
      persist(items.filter((it) => it.id !== id));
    }
    setEditingId(null);
    setEditText("");
  }

  function del(id: string) {
    persist(items.filter((it) => it.id !== id));
    setEditingId(null);
  }

  function addItem() {
    const id = nextId();
    const next: TodoItem[] = [...items, { id, text: "", done: false }];
    persist(next);
    // Enter edit mode for the new row
    setEditingId(id);
    setEditText("");
    requestAnimationFrame(() => inputRef.current?.focus());
  }

  return (
    <div className="todo">
      <div className="todo__head">
        <HubIco d={HP_PATHS.list} size={15} color="var(--pp)" />
        <span>To-Do List</span>
      </div>
      <div className="todo__items">
        {items.map((it) =>
          editingId === it.id ? (
            <div key={it.id} className="todo__item todo__item--edit">
              <input
                ref={inputRef}
                className="todo__input"
                value={editText}
                onChange={(e) => setEditText(e.target.value)}
                onBlur={() => commitEdit(it.id)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") commitEdit(it.id);
                  if (e.key === "Escape") { setEditingId(null); setEditText(""); }
                }}
                placeholder="What needs doing?"
                autoComplete="off"
              />
              <button
                className="todo__del-btn"
                aria-label="Delete item"
                onMouseDown={(e) => { e.preventDefault(); del(it.id); }}
              >
                <HubIco d="M18 6L6 18M6 6l12 12" size={11} color="var(--tc-dim)" />
              </button>
            </div>
          ) : (
            <div key={it.id} className="todo__item-row">
              <button
                className="todo__item"
                data-done={it.done}
                onClick={() => toggle(it.id)}
                aria-label={it.done ? `Mark "${it.text}" not done` : `Mark "${it.text}" done`}
              >
                <span className="todo__box">
                  {it.done && (
                    <HubIco d={HP_PATHS.check} size={11} color="#fff" sw={3} />
                  )}
                </span>
                <span className="todo__text">{it.text || <em>Untitled</em>}</span>
              </button>
              <button
                className="todo__del-btn"
                aria-label="Delete item"
                onClick={() => del(it.id)}
              >
                <HubIco d="M18 6L6 18M6 6l12 12" size={11} color="var(--tc-dim)" />
              </button>
              {!it.done && (
                <button
                  className="todo__edit-btn"
                  aria-label="Edit item"
                  onClick={() => startEdit(it.id, it.text)}
                >
                  <HubIco d={HP_PATHS.pencil} size={11} color="var(--tc-dim)" />
                </button>
              )}
            </div>
          )
        )}
      </div>
      <button className="todo__add" onClick={addItem}>+ Add to my list</button>
    </div>
  );
}
