import { useState } from "react";
import { HubIco } from "./HubIco";
import { HP_PATHS } from "./icons";
import { HOME } from "../data/mockHome";
import type { TodoItem } from "../data/mockHome";

export function TodoWidget() {
  const [items, setItems] = useState<TodoItem[]>(HOME.todos);

  function toggle(idx: number) {
    setItems((prev) => prev.map((it, i) => (i === idx ? { ...it, done: !it.done } : it)));
  }

  return (
    <div className="todo">
      <div className="todo__head">
        <HubIco d={HP_PATHS.list} size={15} color="var(--pp)" />
        <span>To-Do List</span>
      </div>
      <div className="todo__items">
        {items.map((it, i) => (
          <button
            key={i}
            className="todo__item"
            data-done={it.done}
            onClick={() => toggle(i)}
          >
            <span className="todo__box">
              {it.done && (
                <HubIco d={HP_PATHS.check} size={11} color="#fff" sw={3} />
              )}
            </span>
            <span className="todo__text">{it.t}</span>
          </button>
        ))}
      </div>
      <button className="todo__add">+ Add to my list</button>
    </div>
  );
}
