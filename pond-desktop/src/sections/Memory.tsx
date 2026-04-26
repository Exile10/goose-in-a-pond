import { useState, useEffect } from "react";
import { Card, CardContent, Button, Chip, Input } from "@heroui/react";
import { Sparkles, Trash2, BrainCircuit } from "lucide-react";
import { api } from "../api/PondApiClient";
import type { MemoryFragment } from "../api/types";

export function Memory() {
  const [items, setItems]     = useState<MemoryFragment[]>([]);
  const [loading, setLoading] = useState(true);
  const [input, setInput]     = useState("");
  const [saving, setSaving]   = useState(false);
  const [error, setError]     = useState<string | null>(null);

  function load() {
    setLoading(true);
    api
      .listMemories(30)
      .then(setItems)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }

  useEffect(() => {
    load();
  }, []);

  async function add() {
    if (!input.trim()) return;
    setSaving(true);
    try {
      await api.addMemory(input.trim());
      setInput("");
      load();
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  }

  async function remove(id: string) {
    try {
      await api.deleteMemory(id);
      load();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div className="screen">
      {/* ── Page header ─────────────────────────────────────── */}
      <div className="page-header">
        <h1 className="page-header__title">Memory</h1>
        <div className="page-header__action">
          <Chip size="sm" variant="flat" color="secondary">
            {loading ? "..." : items.length}
          </Chip>
        </div>
      </div>

      {/* ── Add memory ──────────────────────────────────────── */}
      <Card shadow="none" className="giap-card">
        <CardContent>
          <div className="mem-add">
            <Input
              className="mem-add__input"
              size="sm"
              radius="md"
              variant="bordered"
              placeholder="Add a memory..."
              aria-label="New memory"
              value={input}
              onValueChange={setInput}
              onKeyDown={(e: React.KeyboardEvent) => e.key === "Enter" && add()}
              startContent={<Sparkles size={14} />}
            />
            <Button
              size="sm"
              color="secondary"
              onPress={add}
              isDisabled={!input.trim() || saving}
            >
              Add
            </Button>
          </div>
        </CardContent>
      </Card>

      {/* ── Error ───────────────────────────────────────────── */}
      {error && (
        <p style={{ color: "var(--color-destructive)", fontSize: "var(--text-sm)", margin: 0 }}>
          {error}
        </p>
      )}

      {/* ── Memory list ─────────────────────────────────────── */}
      {loading ? (
        <p className="muted-12">Loading...</p>
      ) : items.length === 0 ? (
        <div className="empty-state">
          <BrainCircuit size={32} />
          <span>No memories yet. Add one above.</span>
        </div>
      ) : (
        <Card shadow="none" className="giap-card">
          <CardContent className="card-body--list">
            {items.map((m) => (
              <div key={m.id} className="mem-row">
                <div className="mem-row__bullet">
                  <BrainCircuit size={12} />
                </div>
                <div className="mem-row__text">{m.content}</div>
                <div className="mem-row__date">
                  {new Date(m.created_at).toLocaleDateString()}
                </div>
                <Button
                  size="sm"
                  variant="light"
                  color="danger"
                  isIconOnly
                  onPress={() => remove(m.id)}
                  aria-label="Delete memory"
                >
                  <Trash2 size={14} />
                </Button>
              </div>
            ))}
          </CardContent>
        </Card>
      )}
    </div>
  );
}
