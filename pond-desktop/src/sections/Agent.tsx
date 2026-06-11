import { useState, useEffect, useRef } from "react";
import {
  Tabs,
  Button,
  Card,
  CardContent,
  Chip,
} from "@heroui/react";
import { Trash2, Plus, Send, Wrench, Terminal, FileText, ChefHat } from "lucide-react";
import { api } from "../api/PondApiClient";
import { PageHeader, ErrorBanner, SkeletonList } from "../components/shared";
import type { AgentTool, AgentRecipe, PromptExtra, ChatEvent } from "../api/types";

type Tab = "chat" | "tools" | "extras" | "recipes";

export function Agent() {
  const [tab, setTab] = useState<Tab>("chat");

  return (
    <div className="screen screen--agent">
      <PageHeader title="Agent" />

      <Tabs
        selectedKey={tab}
        onSelectionChange={(k) => setTab(k as Tab)}
      >
        <Tabs.ListContainer>
          <Tabs.List aria-label="Agent sections" className="agent-tabs">
            {[
              { id: "chat", label: "Chat" },
              { id: "tools", label: "MCP Tools" },
              { id: "extras", label: "Prompt Extras" },
              { id: "recipes", label: "Recipes" },
            ].map((t) => (
              <Tabs.Tab key={t.id} id={t.id} onClick={() => setTab(t.id as Tab)} className="agent-tab">
                <Tabs.Indicator />
                {t.label}
              </Tabs.Tab>
            ))}
          </Tabs.List>
        </Tabs.ListContainer>
      </Tabs>

      {tab === "chat"    && <AgentChatPanel />}
      {tab === "tools"   && <ToolsPanel />}
      {tab === "extras"  && <ExtrasPanel />}
      {tab === "recipes" && <RecipesPanel />}
    </div>
  );
}

// ── Agent Chat ────────────────────────────────────────────────

type AgentMsg =
  | { kind: "user"; text: string }
  | { kind: "status"; text: string }
  | { kind: "tool_call"; tool: string }
  | { kind: "assistant"; text: string }
  | { kind: "error"; text: string };

function AgentChatPanel() {
  const [messages, setMessages] = useState<AgentMsg[]>([]);
  const [input, setInput]       = useState("");
  const [busy, setBusy]         = useState(false);
  const sessionId               = useRef<string | undefined>(undefined);
  const bottomRef               = useRef<HTMLDivElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  async function send() {
    const text = input.trim();
    if (!text || busy) return;
    setInput("");
    setBusy(true);
    setMessages((m) => [...m, { kind: "user", text }]);

    try {
      for await (const ev of api.agentChatStream(text, sessionId.current)) {
        handleAgentEvent(ev);
      }
    } catch (e) {
      setMessages((m) => [...m, { kind: "error", text: String(e) }]);
    } finally {
      setBusy(false);
    }
  }

  function handleAgentEvent(ev: ChatEvent) {
    if (ev.type === "status" && ev.content) {
      setMessages((m) => [...m, { kind: "status", text: ev.content! }]);
    } else if (ev.type === "tool_call" && ev.tool) {
      setMessages((m) => [...m, { kind: "tool_call", tool: ev.tool! }]);
    } else if (ev.type === "text" && ev.content) {
      setMessages((m) => {
        const last = m[m.length - 1];
        if (last?.kind === "assistant") {
          return [...m.slice(0, -1), { kind: "assistant", text: last.text + ev.content! }];
        }
        return [...m, { kind: "assistant", text: ev.content! }];
      });
    } else if (ev.done && ev.session_id) {
      sessionId.current = ev.session_id;
    } else if (ev.error) {
      setMessages((m) => [...m, { kind: "error", text: ev.error! }]);
    }
  }

  return (
    <Card className="card agent-chat-card">
      <CardContent style={{ display: "flex", flexDirection: "column", gap: "var(--space-3)", minHeight: "420px" }}>
        {/* Messages */}
        <div style={{ flex: 1, overflowY: "auto", display: "flex", flexDirection: "column", gap: 8, paddingRight: 4 }}>
          {messages.length === 0 && (
            <div className="empty-state">
              <Send size={28} />
              <p>Send a message. The agent can use MCP tools to take real actions.</p>
            </div>
          )}
          {messages.map((msg, i) => {
            if (msg.kind === "user") return (
              <div key={i} style={bubbleStyles.user}>{msg.text}</div>
            );
            if (msg.kind === "assistant") return (
              <div key={i} style={bubbleStyles.assistant}><pre style={bubbleStyles.pre}>{msg.text}</pre></div>
            );
            if (msg.kind === "tool_call") return (
              <div key={i} className="tool-call">
                <Wrench size={12} />
                <code>{msg.tool}</code>
              </div>
            );
            if (msg.kind === "status") return (
              <div key={i} style={bubbleStyles.status}>{msg.text}</div>
            );
            if (msg.kind === "error") return (
              <div key={i} style={{ ...bubbleStyles.status, color: "var(--color-destructive)" }}>{msg.text}</div>
            );
            return null;
          })}
          {busy && <div style={bubbleStyles.status}>Agent working...</div>}
          <div ref={bottomRef} />
        </div>

        {/* Composer */}
        <div className="agent-chat-card__composer">
          <input
            style={{ ...composerInput, flex: 1 }}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(); } }}
            placeholder="Ask the agent..."
            disabled={busy}
            aria-label="Agent message"
          />
          <Button variant="primary" onPress={send} isDisabled={busy || !input.trim()}>
            <Send size={14} />
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}

// ── MCP Tools ────────────────────────────────────────────────

function ToolsPanel() {
  const [tools, setTools]     = useState<AgentTool[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError]     = useState<string | null>(null);

  useEffect(() => {
    api.listTools().then(setTools).catch((e) => setError(String(e))).finally(() => setLoading(false));
  }, []);

  function reload() { setError(null); setLoading(true); api.listTools().then(setTools).catch((e) => setError(String(e))).finally(() => setLoading(false)); }

  if (loading) return <SkeletonList rows={5} />;
  if (error)   return <ErrorBanner error={error} onRetry={reload} />;
  if (!tools.length) return (
    <div className="empty-state">
      <Wrench size={28} />
      <p>No MCP tools loaded. Start pond-server with an extension enabled.</p>
    </div>
  );

  // Group tools by extension
  const byExtension: Record<string, AgentTool[]> = {};
  for (const t of tools) {
    (byExtension[t.extension] ??= []).push(t);
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12, paddingTop: 8 }}>
      {Object.entries(byExtension).map(([ext, extTools]) => (
        <Card key={ext} className="card">
          <CardContent className="card-body--flush">
            <div style={{ display: "flex", alignItems: "center", gap: 10, padding: "12px 14px", borderBottom: "1px solid var(--grey-200)", background: "var(--grey-50)" }}>
              <Chip variant="primary" size="sm">{ext}</Chip>
              <span className="muted-12" style={{ marginLeft: "auto" }}>
                {extTools.length} tool{extTools.length !== 1 ? "s" : ""}
              </span>
            </div>
            <div>
              {extTools.map((t) => (
                <div key={t.name} className="tool-row">
                  <span className="tool-row__icon"><Terminal size={14} /></span>
                  <span className="tool-row__name">{t.name.replace(`${ext}__`, "")}</span>
                  {t.description && (
                    <Chip size="sm" variant="soft" style={{ marginLeft: 6 }}>
                      {t.description.length > 50 ? t.description.slice(0, 47) + "..." : t.description}
                    </Chip>
                  )}
                  <span className="tool-row__spacer" />
                </div>
              ))}
            </div>
          </CardContent>
        </Card>
      ))}
    </div>
  );
}

// ── Prompt Extras ────────────────────────────────────────────

function ExtrasPanel() {
  const [extras, setExtras]     = useState<PromptExtra[]>([]);
  const [loading, setLoading]   = useState(true);
  const [key, setKey]           = useState("");
  const [content, setContent]   = useState("");
  const [saving, setSaving]     = useState(false);
  const [error, setError]       = useState<string | null>(null);

  function load() {
    setLoading(true);
    api.listExtras().then(setExtras).catch((e) => setError(String(e))).finally(() => setLoading(false));
  }

  useEffect(() => { load(); }, []);

  async function add() {
    if (!key.trim() || !content.trim()) return;
    setSaving(true);
    try {
      await api.addExtra(key.trim(), content.trim());
      setKey(""); setContent(""); load();
    } catch (e) { setError(String(e)); } finally { setSaving(false); }
  }

  async function remove(k: string) {
    try { await api.deleteExtra(k); load(); } catch (e) { setError(String(e)); }
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12, paddingTop: 8 }}>
      {/* Add form */}
      <div className="extras-add">
        <div>
          <input
            style={composerInput}
            value={key}
            onChange={(e) => setKey(e.target.value)}
            placeholder="Key (e.g. context_note)"
            aria-label="Extra key"
          />
        </div>
        <Button variant="primary" onPress={add} isDisabled={saving || !key.trim() || !content.trim()}>
          <Plus size={14} />
          Add
        </Button>
      </div>
      <textarea
        style={{ ...composerInput, height: "64px", resize: "vertical" as const, padding: "8px 12px" }}
        value={content}
        onChange={(e) => setContent(e.target.value)}
        placeholder="Content injected into system prompt..."
        aria-label="Extra content"
      />

      {error && <ErrorBanner error={error} onRetry={load} />}

      {loading ? (
        <SkeletonList rows={3} />
      ) : extras.length === 0 ? (
        <div className="empty-state--inline">
          <FileText size={18} />
          <span>No prompt extras. Extras are injected into every agent system prompt.</span>
        </div>
      ) : (
        <Card className="card">
          <CardContent className="card-body--flush">
            {extras.map((ex) => (
              <div key={ex.key} className="extra-row">
                <code style={{ fontFamily: "var(--font-mono)", fontSize: "var(--text-base)", color: "var(--color-accent)", fontWeight: 600 }}>
                  {ex.key}
                </code>
                {!ex.enabled && <Chip size="sm" variant="soft" color="warning">Disabled</Chip>}
                <span style={{ flex: 1, fontSize: "var(--text-sm)", color: "var(--grey-600)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" as const }}>
                  {ex.content.length > 80 ? ex.content.slice(0, 77) + "..." : ex.content}
                </span>
                <Button
                  variant="danger-soft"
                  onPress={() => remove(ex.key)}
                  aria-label="Delete extra"
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

// ── Recipes ──────────────────────────────────────────────────

function RecipesPanel() {
  const [recipes, setRecipes]   = useState<AgentRecipe[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [loading, setLoading]   = useState(true);
  const [error, setError]       = useState<string | null>(null);

  useEffect(() => {
    api.listRecipes()
      .then((r) => { setRecipes(r); if (r.length) setSelected(r[0].name); })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  const current = recipes.find((r) => r.name === selected);

  function reloadRecipes() { setError(null); setLoading(true); api.listRecipes().then((r) => { setRecipes(r); if (r.length) setSelected(r[0].name); }).catch((e) => setError(String(e))).finally(() => setLoading(false)); }

  if (loading) return <SkeletonList rows={3} />;
  if (error)   return <ErrorBanner error={error} onRetry={reloadRecipes} />;
  if (!recipes.length) return (
    <Card className="card">
      <CardContent>
        <div className="empty-state">
          <ChefHat size={28} />
          <p>No recipes found.</p>
          <p className="muted-12">
            Import one with{" "}
            <Chip size="sm" variant="soft">
              <code style={{ fontFamily: "var(--font-mono)", fontSize: "11px" }}>
                pond-server recipes import &lt;name&gt; &lt;file.yaml&gt;
              </code>
            </Chip>
          </p>
        </div>
      </CardContent>
    </Card>
  );

  return (
    <div style={{ display: "flex", gap: 12, paddingTop: 8, minHeight: 320 }}>
      {/* Recipe list */}
      <Card className="card" style={{ width: 200, flexShrink: 0 }}>
        <CardContent className="card-body--list" style={{ padding: 4 }}>
          {recipes.map((r) => (
            <button
              key={r.name}
              className={`prompt-tab${selected === r.name ? " is-active" : ""}`}
              onClick={() => setSelected(r.name)}
              style={{ width: "100%", textAlign: "left" }}
            >
              <div className="prompt-tab__title">
                <span>{r.name}</span>
                {r.description && <span className="prompt-tab__desc">{r.description}</span>}
              </div>
            </button>
          ))}
        </CardContent>
      </Card>

      {/* Recipe detail */}
      <Card className="card" style={{ flex: 1, minWidth: 0 }}>
        <CardContent style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          {current ? (
            <>
              <div>
                <span style={{ fontWeight: 600, fontSize: "var(--text-base)" }}>{current.name}</span>
                {current.description && (
                  <p style={{ margin: "4px 0 0", fontSize: "var(--text-sm)", color: "var(--grey-600)" }}>{current.description}</p>
                )}
              </div>
              <pre style={{
                flex: 1,
                margin: 0,
                padding: "var(--space-4)",
                background: "var(--grey-50)",
                border: "1px solid var(--grey-200)",
                borderRadius: "var(--radius-card)",
                fontFamily: "var(--font-mono)",
                fontSize: "var(--text-xs)",
                color: "var(--fg)",
                lineHeight: "1.6",
                overflow: "auto",
                whiteSpace: "pre" as const,
              }}>{current.yaml}</pre>
            </>
          ) : (
            <p className="muted-12">Select a recipe to view its YAML.</p>
          )}
        </CardContent>
      </Card>
    </div>
  );
}

// ── Shared input style ─────────────────────────────────────────

const composerInput: React.CSSProperties = {
  height: "36px",
  border: "1px solid var(--grey-200)",
  borderRadius: "var(--radius-card)",
  padding: "0 12px",
  fontSize: "var(--text-base)",
  fontFamily: "var(--font-body)",
  background: "#fff",
  color: "var(--fg)",
  width: "100%",
};

// ── Chat bubble styles ─────────────────────────────────────────

const bubbleStyles: Record<string, React.CSSProperties> = {
  user: {
    alignSelf: "flex-end",
    background: "var(--color-accent)",
    color: "#fff",
    borderRadius: "var(--radius-card) var(--radius-card) 2px var(--radius-card)",
    padding: "8px 12px",
    fontSize: "var(--text-sm)",
    maxWidth: "80%",
    whiteSpace: "pre-wrap",
  },
  assistant: {
    alignSelf: "flex-start",
    background: "#fff",
    border: "1px solid var(--grey-200)",
    borderRadius: "2px var(--radius-card) var(--radius-card) var(--radius-card)",
    padding: "8px 12px",
    fontSize: "var(--text-sm)",
    maxWidth: "90%",
  },
  pre: {
    margin: 0,
    fontFamily: "inherit",
    whiteSpace: "pre-wrap",
    lineHeight: "1.55",
  },
  status: {
    alignSelf: "flex-start",
    fontSize: "var(--text-xs)",
    color: "var(--grey-500)",
    fontStyle: "italic",
  },
};
