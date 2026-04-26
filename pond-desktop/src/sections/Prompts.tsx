import { useState, useEffect } from "react";
import { Button, Card, CardContent, Chip } from "@heroui/react";
import { RotateCcw, Save } from "lucide-react";
import { api } from "../api/PondApiClient";
import type { PromptTemplate } from "../api/types";

// ── Preset descriptions (keyed by common template names) ─────
const PRESET_META: Record<string, string> = {
  system:   "Core system prompt sent with every request",
  chat:     "Lightweight prompt for casual conversation",
  think:    "Reasoning prompt for analytical tasks",
  task:     "Action-oriented prompt for tool use & tasks",
};

/** Rough token estimate: ~4 chars per token. */
function estimateTokens(text: string): number {
  return Math.ceil(text.length / 4);
}

/** Known template variables the backend interpolates. */
const TEMPLATE_VARS = [
  "{{user_name}}",
  "{{assistant_name}}",
  "{{personality}}",
  "{{location}}",
  "{{datetime}}",
  "{{timezone}}",
  "{{memory}}",
];

export function Prompts() {
  const [prompts, setPrompts]   = useState<PromptTemplate[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [content, setContent]   = useState("");
  const [original, setOriginal] = useState("");
  const [loading, setLoading]   = useState(true);
  const [saving, setSaving]     = useState(false);
  const [error, setError]       = useState<string | null>(null);

  useEffect(() => {
    api.listPrompts()
      .then((p) => {
        const list = Array.isArray(p) ? p : [];
        setPrompts(list);
        if (list.length) select(list[0].name, list);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  function select(name: string, list?: PromptTemplate[]) {
    setSelected(name);
    const src = list ?? prompts;
    const p = src.find((x) => x.name === name);
    if (p) {
      setContent(p.content);
      setOriginal(p.content);
    }
  }

  function reset() {
    setContent(original);
  }

  async function save() {
    if (!selected) return;
    setSaving(true);
    try {
      const updated = await api.updatePrompt(selected, content);
      setPrompts((prev) => prev.map((p) => p.name === selected ? updated : p));
      setOriginal(content);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  }

  if (loading) return <p className="muted-12">Loading...</p>;
  if (error)   return <p className="muted-12" style={{ color: "var(--color-destructive)" }}>{error}</p>;
  if (!prompts.length) return (
    <p className="muted-12">
      No prompt templates found. Prompts are created automatically when the agent runs for the first time.
    </p>
  );

  const dirty = content !== original;
  const tokens = estimateTokens(content);

  return (
    <div className="screen">
      {/* Page header */}
      <div className="page-header">
        <h2 className="page-header__title">Prompts</h2>
        <Chip size="sm" variant="soft">{prompts.length} preset{prompts.length !== 1 ? "s" : ""}</Chip>
      </div>

      {/* Prompt tab grid */}
      <div className="prompt-tabs">
        {prompts.map((p) => (
          <button
            key={p.name}
            className={`prompt-tab${selected === p.name ? " is-active" : ""}`}
            onClick={() => select(p.name)}
          >
            <div className="prompt-tab__title">
              <span>{p.name}</span>
              <span className="prompt-tab__desc">
                {PRESET_META[p.name] ?? "Custom template"}
              </span>
            </div>
          </button>
        ))}
      </div>

      {/* Editor */}
      {selected && (
        <Card shadow="none" className="giap-card">
          <CardContent>
            <textarea
              className="prompt-area"
              value={content}
              onChange={(e) => setContent(e.target.value)}
              aria-label="Prompt editor"
              rows={16}
              spellCheck={false}
              style={{
                width: "100%",
                border: "1px solid var(--grey-200)",
                borderRadius: "var(--radius-card)",
                padding: "12px",
                background: "#fff",
                color: "var(--fg)",
                resize: "vertical",
                lineHeight: "1.55",
              }}
            />
          </CardContent>
        </Card>
      )}

      {/* Footer */}
      {selected && (
        <div className="prompt-foot">
          <div className="prompt-foot__tokens">
            <span>~{tokens.toLocaleString()} tokens</span>
            {dirty && <Chip size="sm" color="warning" variant="soft">Unsaved</Chip>}
          </div>
          <div className="prompt-foot__actions">
            <Button
              variant="outline"
              onPress={reset}
              isDisabled={!dirty}
            >
              <RotateCcw size={14} />
              Reset
            </Button>
            <Button
              variant="primary"
              onPress={save}
              isDisabled={saving || !dirty}
            >
              <Save size={14} />
              {saving ? "Saving..." : "Save"}
            </Button>
          </div>
        </div>
      )}

      {/* Variables reference */}
      <Card shadow="none" className="giap-card">
        <CardContent>
          <span className="card__label">Variables you can use</span>
          <div className="var-grid">
            {TEMPLATE_VARS.map((v) => (
              <Chip key={v} size="sm" variant="soft">
                <code style={{ fontFamily: "var(--font-mono)", fontSize: "11px" }}>{v}</code>
              </Chip>
            ))}
          </div>
        </CardContent>
      </Card>
    </div>
  );
}
