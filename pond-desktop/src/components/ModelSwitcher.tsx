import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Check, ChevronDown, Cpu, Loader2 } from "lucide-react";
import { api } from "../api/PondApiClient";
import { useAppDispatch } from "../state/AppContext";
import type { ModelActiveRoles, ModelEngine, ModelEntry } from "../api/types";
import { EngineMark } from "./models/ModelMarks";
import { engineOf } from "../lib/modelProvider";
import {
  formatSize, holderEntry, isInUse, modelFacts, modelFullName, modelLabel, roleHolder, rolesFor,
  sortModels,
} from "../sections/models/modelsView";

// The composer's model chip: which model is answering, and a way to change it. It names the
// model and the engine that runs it, lists only models that can hold a conversation, groups them
// by engine, and says so when a switch fails.

const MESH_LABEL = "Mesh (trusted peer)";

/** The engine order a household meets them in; an engine this build does not know sorts last. */
const ENGINE_ORDER = ["llama_cpp", "litert_lm", "ollama", "llamafile"];

interface Group {
  engine: ModelEngine;
  models: ModelEntry[];
}

function groupsOf(models: ModelEntry[]): Group[] {
  const byEngine = new Map<string, Group>();
  for (const m of sortModels(models)) {
    const engine = engineOf(m);
    if (!engine) continue;
    const group = byEngine.get(engine.id) ?? { engine, models: [] };
    group.models.push(m);
    byEngine.set(engine.id, group);
  }
  const rank = (id: string) => {
    const i = ENGINE_ORDER.indexOf(id);
    return i === -1 ? ENGINE_ORDER.length : i;
  };
  return [...byEngine.values()].sort((a, b) => rank(a.engine.id) - rank(b.engine.id));
}

export function ModelSwitcher({ onSwitched }: { onSwitched?: () => void }) {
  const dispatch = useAppDispatch();
  const [open, setOpen] = useState(false);
  const [models, setModels] = useState<ModelEntry[] | null>(null);
  const [roles, setRoles] = useState<ModelActiveRoles | null>(null);
  const [provider, setProvider] = useState<string | null>(null);
  const [meshEnabled, setMeshEnabled] = useState(false);
  const [switching, setSwitching] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const wrapRef = useRef<HTMLDivElement>(null);

  /** Each read stands alone: a model list that fails still leaves the label the roles give. A call
   *  that throws before it returns a promise reads as a failed one. */
  const load = useCallback(async () => {
    const [list, assigned, settings] = await Promise.allSettled([
      Promise.resolve().then(() => api.listModels()),
      Promise.resolve().then(() => api.getActiveRoles()),
      Promise.resolve().then(() => api.getSettings()),
    ]);
    if (list.status === "fulfilled") {
      setModels(list.value);
      setProblem((p) => (p?.startsWith("Could not read") ? null : p));
    } else {
      setProblem("Could not read the model list. Try again in a moment.");
    }
    if (assigned.status === "fulfilled") setRoles(assigned.value);
    if (settings.status === "fulfilled") {
      setProvider(settings.value.chat_provider ?? null);
      setMeshEnabled(settings.value.mesh_enabled ?? false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (!open) return;
    function onClick(e: MouseEvent) {
      if (wrapRef.current && !wrapRef.current.contains(e.target as Node)) setOpen(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("mousedown", onClick);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onClick);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const onMesh = provider === "mesh";
  const holder = useMemo(
    () => (models ? holderEntry(models, roles, "chat") : null),
    [models, roles],
  );
  const named = roleHolder(roles, "chat");

  const label = switching
    ? "Switching…"
    : onMesh
      ? MESH_LABEL
      : holder
        ? `${modelLabel(holder)}${engineOf(holder) ? ` · ${engineOf(holder)!.label}` : ""}`
        : named
          ? named
          : roles || models
            ? "Choose a model"
            : "Model";

  const conversation = useMemo(
    () => (models ?? []).filter((m) => m.downloaded !== false && rolesFor(m).includes("chat")),
    [models],
  );
  const groups = useMemo(() => groupsOf(conversation), [conversation]);

  function toggle() {
    if (open) {
      setOpen(false);
      return;
    }
    setOpen(true);
    setProblem(null);
    void load();
  }

  async function choose(m: ModelEntry) {
    setSwitching(true);
    setProblem(null);
    try {
      await api.activateModel(m.category ?? m.provider, m.name, "chat");
      dispatch({
        type: "SET_LAST_RESPONSE_META",
        payload: { modelName: m.name, modelRole: "chat", completionTokens: 0 },
      });
      setOpen(false);
      await load();
      // The new model may have a different encoder, or none; don't wait for the next poll.
      onSwitched?.();
    } catch (e) {
      setProblem(`Could not switch to ${modelLabel(m)}: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setSwitching(false);
    }
  }

  async function chooseMesh() {
    setSwitching(true);
    setProblem(null);
    try {
      // The mesh has no catalogue row to activate, so the provider is set directly.
      await api.updateSettings({ chat_provider: "mesh" });
      setProvider("mesh");
      setOpen(false);
      onSwitched?.();
    } catch (e) {
      setProblem(`Could not switch to the mesh: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setSwitching(false);
    }
  }

  return (
    <div ref={wrapRef} className="model-selector-wrap">
      <button
        className={`model-selector-trigger reach${open ? " is-open" : ""}`}
        onClick={toggle}
        disabled={switching}
        aria-label="Select model"
        aria-haspopup="true"
        aria-expanded={open}
        type="button"
      >
        <Cpu size={12} aria-hidden="true" />
        <span className="model-selector-trigger__label">{label}</span>
        {switching ? <Loader2 size={11} className="spin" aria-hidden="true" /> : <ChevronDown size={11} aria-hidden="true" />}
      </button>

      {open && (
        <div className="model-selector-dropdown">
          <div className="model-selector-dropdown__header">
            <span>Switch model</span>
          </div>
          {problem && (
            <p className="model-selector-dropdown__problem" role="alert">
              {problem}
            </p>
          )}
          <div className="model-selector-dropdown__list">
            {groups.length === 0 && !meshEnabled && (
              <div className="model-selector-dropdown__empty">
                No model is on this device yet.
                <button
                  type="button"
                  className="model-selector-dropdown__link"
                  onClick={() => {
                    setOpen(false);
                    dispatch({ type: "SET_SECTION", payload: "models" });
                  }}
                >
                  Open Models
                </button>
              </div>
            )}
            {groups.map(({ engine, models: rows }) => (
              <div key={engine.id} role="group" aria-label={engine.label}>
                <div className="model-selector-dropdown__group-label">
                  <EngineMark engine={engine} label={engine.label} format={engine.file_format} />
                </div>
                {rows.map((m) => {
                  const active = !onMesh && isInUse(m, roles);
                  const facts = [formatSize(m.size_mb), ...modelFacts(m).slice(0, 1)].filter(Boolean);
                  return (
                    <button
                      key={m.id}
                      type="button"
                      className={`model-selector-dropdown__item${active ? " is-active" : ""}`}
                      onClick={() => void choose(m)}
                      disabled={switching}
                      aria-current={active ? "true" : undefined}
                      aria-label={`${modelFullName(m)}${active ? ", in use" : ""}`}
                    >
                      <span className="model-selector-dropdown__item-name">{modelLabel(m)}</span>
                      <span className="model-selector-dropdown__item-meta">
                        {facts.join(" · ")}
                        {active && <Check size={13} aria-hidden="true" />}
                      </span>
                    </button>
                  );
                })}
              </div>
            ))}
            {meshEnabled && (
              <div role="group" aria-label="Mesh">
                <div className="model-selector-dropdown__group-label">Mesh</div>
                <button
                  type="button"
                  className={`model-selector-dropdown__item${onMesh ? " is-active" : ""}`}
                  onClick={() => void chooseMesh()}
                  disabled={switching}
                  aria-current={onMesh ? "true" : undefined}
                >
                  <span className="model-selector-dropdown__item-name">{MESH_LABEL}</span>
                  <span className="model-selector-dropdown__item-meta">
                    {onMesh && <Check size={13} aria-hidden="true" />}
                  </span>
                </button>
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
