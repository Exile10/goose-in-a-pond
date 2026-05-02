import { useState, useEffect, useCallback } from "react";
import {
  Button,
  Card,
  CardContent,
  Switch,
} from "@heroui/react";
import {
  Puzzle,
  Trash2,
  ChevronDown,
  ChevronUp,
  Plus,
  RefreshCw,
  Wrench,
} from "lucide-react";
import { api } from "../api/PondApiClient";
import { useAppState } from "../state/AppContext";
import type { Extension, AddExtensionRequest } from "../api/types";

// ── Extension Card ────────────────────────────────────────────

function ExtensionCard({
  ext,
  onToggle,
  onDelete,
  disabled,
}: {
  ext: Extension;
  onToggle: (name: string, enabled: boolean) => void;
  onDelete: (name: string) => void;
  disabled: boolean;
}) {
  const [expanded, setExpanded] = useState(false);

  return (
    <div className={`ext-card${ext.enabled ? " ext-card--enabled" : " ext-card--disabled"}`}>
      <div className="ext-card__header">
        {/* Icon + info */}
        <div className="ext-card__icon">
          <Puzzle size={15} strokeWidth={1.8} />
        </div>
        <div className="ext-card__info">
          <div className="ext-card__title-row">
            <span className="ext-card__name">{ext.name}</span>
            <span className={`ext-card__kind-badge ext-card__kind-badge--${ext.kind === "stdio" ? "stdio" : "http"}`}>
              {ext.kind}
            </span>
            {ext.tools.length > 0 && (
              <span className="ext-card__tool-count">
                {ext.tools.length} {ext.tools.length === 1 ? "tool" : "tools"}
              </span>
            )}
          </div>
          {ext.description && (
            <div className="ext-card__desc">{ext.description}</div>
          )}
        </div>

        {/* Actions */}
        <div className="ext-card__actions">
          {ext.tools.length > 0 && (
            <button
              className="ext-card__expand-btn"
              onClick={() => setExpanded((v) => !v)}
              aria-label={expanded ? "Collapse tools" : "Expand tools"}
              aria-expanded={expanded}
            >
              {expanded ? <ChevronUp size={14} strokeWidth={1.8} /> : <ChevronDown size={14} strokeWidth={1.8} />}
            </button>
          )}
          <Switch
            isSelected={ext.enabled}
            onValueChange={(val) => onToggle(ext.name, val)}
            isDisabled={disabled}
            size="sm"
            aria-label={`${ext.enabled ? "Disable" : "Enable"} ${ext.name}`}
          />
          <button
            className="ext-card__delete-btn"
            onClick={() => onDelete(ext.name)}
            disabled={disabled}
            aria-label={`Delete ${ext.name}`}
            title={`Delete ${ext.name}`}
          >
            <Trash2 size={13} strokeWidth={1.8} />
          </button>
        </div>
      </div>

      {/* Expandable tools list */}
      {expanded && ext.tools.length > 0 && (
        <div className="ext-card__tools">
          <div className="ext-card__tools-header">
            <Wrench size={11} strokeWidth={1.8} />
            <span>Available tools</span>
          </div>
          <div className="ext-card__tool-list">
            {ext.tools.map((tool) => (
              <div key={tool} className="ext-card__tool-item">
                <code>{tool}</code>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

// ── Add Extension Form ────────────────────────────────────────

type ExtKind = "stdio" | "streamable_http";

function AddExtensionForm({
  onAdd,
  disabled,
}: {
  onAdd: (req: AddExtensionRequest) => Promise<void>;
  disabled: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [name, setName] = useState("");
  const [kind, setKind] = useState<ExtKind>("stdio");
  const [command, setCommand] = useState("");
  const [args, setArgs] = useState("");
  const [uri, setUri] = useState("");

  function reset() {
    setName("");
    setKind("stdio");
    setCommand("");
    setArgs("");
    setUri("");
    setError(null);
  }

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!name.trim()) { setError("Name is required."); return; }
    if (kind === "stdio" && !command.trim()) { setError("Command is required for stdio extensions."); return; }
    if (kind === "streamable_http" && !uri.trim()) { setError("URI is required for HTTP extensions."); return; }

    const req: AddExtensionRequest = {
      name: name.trim(),
      kind,
      ...(kind === "stdio" && {
        command: command.trim(),
        args: args.trim() ? args.split(",").map((a) => a.trim()).filter(Boolean) : [],
      }),
      ...(kind === "streamable_http" && { uri: uri.trim() }),
    };

    setSubmitting(true);
    setError(null);
    try {
      await onAdd(req);
      reset();
      setOpen(false);
    } catch (err) {
      setError(String(err));
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <div className="ext-add-accordion">
      <button
        className="ext-add-accordion__trigger"
        onClick={() => { setOpen((v) => !v); if (!open) setError(null); }}
        aria-expanded={open}
      >
        <span className="ext-add-accordion__trigger-label">
          <Plus size={13} strokeWidth={2} />
          Add Extension
        </span>
        {open ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
      </button>

      {open && (
        <form className="ext-add-form" onSubmit={handleSubmit} noValidate>
          {/* Name */}
          <div className="ext-form-row">
            <label className="ext-form-label" htmlFor="ext-name">
              Name <span className="ext-form-required">*</span>
            </label>
            <input
              id="ext-name"
              className="ext-form-input"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="my-extension"
              required
              autoComplete="off"
              disabled={disabled || submitting}
            />
          </div>

          {/* Kind */}
          <div className="ext-form-row">
            <label className="ext-form-label" htmlFor="ext-kind">Kind</label>
            <select
              id="ext-kind"
              className="ext-form-select"
              value={kind}
              onChange={(e) => setKind(e.target.value as ExtKind)}
              disabled={disabled || submitting}
            >
              <option value="stdio">stdio</option>
              <option value="streamable_http">streamable_http</option>
            </select>
          </div>

          {/* Conditional: stdio */}
          {kind === "stdio" && (
            <>
              <div className="ext-form-row">
                <label className="ext-form-label" htmlFor="ext-command">
                  Command <span className="ext-form-required">*</span>
                </label>
                <input
                  id="ext-command"
                  className="ext-form-input"
                  value={command}
                  onChange={(e) => setCommand(e.target.value)}
                  placeholder="/usr/local/bin/my-mcp-server"
                  required
                  autoComplete="off"
                  disabled={disabled || submitting}
                />
              </div>
              <div className="ext-form-row">
                <label className="ext-form-label" htmlFor="ext-args">
                  Args
                  <span className="ext-form-hint"> (comma-separated)</span>
                </label>
                <input
                  id="ext-args"
                  className="ext-form-input"
                  value={args}
                  onChange={(e) => setArgs(e.target.value)}
                  placeholder="--port, 8080, --verbose"
                  autoComplete="off"
                  disabled={disabled || submitting}
                />
              </div>
            </>
          )}

          {/* Conditional: http */}
          {kind === "streamable_http" && (
            <div className="ext-form-row">
              <label className="ext-form-label" htmlFor="ext-uri">
                URI <span className="ext-form-required">*</span>
              </label>
              <input
                id="ext-uri"
                className="ext-form-input"
                value={uri}
                onChange={(e) => setUri(e.target.value)}
                placeholder="http://localhost:3001/mcp"
                type="url"
                required
                autoComplete="off"
                disabled={disabled || submitting}
              />
            </div>
          )}

          {/* Error */}
          {error && (
            <p className="ext-form-error">{error}</p>
          )}

          {/* Actions */}
          <div className="ext-form-actions">
            <Button
              variant="ghost"
              size="sm"
              onPress={() => { setOpen(false); reset(); }}
              isDisabled={submitting}
              type="button"
            >
              Cancel
            </Button>
            <Button
              variant="secondary"
              size="sm"
              type="submit"
              isDisabled={disabled || submitting}
            >
              {submitting ? "Adding…" : "Add Extension"}
            </Button>
          </div>
        </form>
      )}
    </div>
  );
}

// ── Main Extensions Component ─────────────────────────────────

export function Extensions() {
  const state = useAppState();
  const [extensions, setExtensions] = useState<Extension[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [actionMsg, setActionMsg] = useState<{ text: string; ok: boolean } | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await api.listExtensions();
      // Backend returns { extensions: [...] } or plain array
      const list = Array.isArray(res)
        ? (res as Extension[])
        : ((res as { extensions: Extension[] }).extensions ?? []);
      setExtensions(list);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { load(); }, [load]);

  function flash(text: string, ok = true) {
    setActionMsg({ text, ok });
    setTimeout(() => setActionMsg(null), 3500);
  }

  async function handleToggle(name: string, enabled: boolean) {
    try {
      await api.toggleExtension(name, enabled);
      setExtensions((prev) =>
        prev.map((e) => e.name === name ? { ...e, enabled } : e),
      );
      flash(`${name} ${enabled ? "enabled" : "disabled"}.`);
    } catch (err) {
      flash(String(err), false);
    }
  }

  async function handleDelete(name: string) {
    if (!confirm(`Delete extension "${name}"? This cannot be undone.`)) return;
    try {
      await api.removeExtension(name);
      setExtensions((prev) => prev.filter((e) => e.name !== name));
      flash(`${name} removed.`);
    } catch (err) {
      flash(String(err), false);
    }
  }

  async function handleAdd(req: AddExtensionRequest) {
    const ext = await api.addExtension(req);
    setExtensions((prev) => [...prev, ext]);
    flash(`${ext.name} added.`);
  }

  const enabledCount = extensions.filter((e) => e.enabled).length;

  return (
    <div className="screen">
      {/* Page header */}
      <div className="page-header">
        <h1 className="page-header__title">Extensions</h1>
        <div className="page-header__action">
          <Button size="sm" variant="ghost" onPress={load} isDisabled={loading}>
            <RefreshCw size={14} strokeWidth={1.8} style={{ opacity: loading ? 0.4 : 1 }} />
            Refresh
          </Button>
        </div>
      </div>

      {/* Summary banner */}
      <div className="seg-banner seg-banner--info">
        <Puzzle size={14} />
        <span>
          MCP server extensions give the agent additional tools.
          {!loading && (
            <> {extensions.length} registered, {enabledCount} active.</>
          )}
        </span>
      </div>

      {/* Action feedback */}
      {actionMsg && (
        <p
          style={{
            margin: 0,
            fontSize: "var(--text-sm)",
            color: actionMsg.ok ? "var(--color-success)" : "var(--color-destructive)",
          }}
        >
          {actionMsg.text}
        </p>
      )}

      {/* Extensions list */}
      <Card shadow="none" className="giap-card">
        <CardContent>
          {loading && (
            <p style={{ margin: 0, fontSize: "var(--text-sm)", color: "var(--color-text-tertiary)" }}>
              Loading extensions…
            </p>
          )}
          {!loading && error && (
            <p style={{ margin: 0, fontSize: "var(--text-sm)", color: "var(--color-destructive)" }}>
              {error}
            </p>
          )}
          {!loading && !error && extensions.length === 0 && (
            <div className="empty-state">
              <Puzzle size={32} strokeWidth={1.2} />
              <div>
                <div style={{ fontWeight: "var(--weight-semibold)", marginBottom: 4 }}>
                  No extensions registered
                </div>
                <div style={{ fontSize: "var(--text-sm)", color: "var(--grey-500)" }}>
                  Add an MCP server extension below to give the agent new capabilities.
                </div>
              </div>
            </div>
          )}
          {!loading && !error && extensions.length > 0 && (
            <div className="ext-list-stack">
              {extensions.map((ext) => (
                <ExtensionCard
                  key={ext.name}
                  ext={ext}
                  onToggle={handleToggle}
                  onDelete={handleDelete}
                  disabled={!state.serverOnline}
                />
              ))}
            </div>
          )}
        </CardContent>
      </Card>

      {/* Add form */}
      <AddExtensionForm onAdd={handleAdd} disabled={!state.serverOnline} />
    </div>
  );
}
