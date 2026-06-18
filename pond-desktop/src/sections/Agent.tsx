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
      <CardContent className="agent-chat-card__card-body">
        {/* Messages */}
        <div className="agent-chat-card__messages">
          {messages.length === 0 && (
            <div className="empty-state">
              <Send size={28} />
              <p>Send a message. The agent can use MCP tools to take real actions.</p>
            </div>
          )}
          {messages.map((msg, i) => {
            if (msg.kind === "user") return (
              <div key={i} className="agent-bubble--user">{msg.text}</div>
            );
            if (msg.kind === "assistant") return (
              <div key={i} className="agent-bubble--assistant"><pre className="agent-bubble__pre">{msg.text}</pre></div>
            );
            if (msg.kind === "tool_call") return (
              <div key={i} className="tool-call">
                <Wrench size={12} />
                <code>{msg.tool}</code>
              </div>
            );
            if (msg.kind === "status") return (
              <div key={i} className="agent-bubble--status">{msg.text}</div>
            );
            if (msg.kind === "error") return (
              <div key={i} className="agent-bubble--error">{msg.text}</div>
            );
            return null;
          })}
          {busy && <div className="agent-bubble--status">Agent working...</div>}
          <div ref={bottomRef} />
        </div>

        {/* Composer */}
        <div className="agent-chat-card__composer">
          <input
            className="agent-input agent-input--flex"
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
    <div className="agent-panel">
      {Object.entries(byExtension).map(([ext, extTools]) => (
        <Card key={ext} className="card">
          <CardContent className="card-body--flush">
            <div className="agent-ext-header">
              <Chip variant="primary" size="sm">{ext}</Chip>
              <span className="muted-12 agent-ext-count">
                {extTools.length} tool{extTools.length !== 1 ? "s" : ""}
              </span>
            </div>
            <div>
              {extTools.map((t) => (
                <div key={t.name} className="tool-row">
                  <span className="tool-row__icon"><Terminal size={14} /></span>
                  <span className="tool-row__name">{t.name.replace(`${ext}__`, "")}</span>
                  {t.description && (
                    <Chip size="sm" variant="soft" className="tool-desc-chip">
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
    <div className="agent-panel">
      {/* Add form */}
      <div className="extras-add">
        <div>
          <input
            className="agent-input"
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
        className="agent-input agent-input--textarea"
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
                <code className="agent-extra-key">
                  {ex.key}
                </code>
                {!ex.enabled && <Chip size="sm" variant="soft" color="warning">Disabled</Chip>}
                <span className="agent-extra-preview">
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
              <code className="agent-recipe__import-code">
                pond-server recipes import &lt;name&gt; &lt;file.yaml&gt;
              </code>
            </Chip>
          </p>
        </div>
      </CardContent>
    </Card>
  );

  return (
    <div className="agent-recipes">
      {/* Recipe list */}
      <Card className="card agent-recipes__list">
        <CardContent className="card-body--list agent-recipes__list-body">
          {recipes.map((r) => (
            <button
              key={r.name}
              className={`prompt-tab agent-recipe-btn${selected === r.name ? " is-active" : ""}`}
              onClick={() => setSelected(r.name)}
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
      <Card className="card agent-recipes__detail">
        <CardContent className="agent-recipes__detail-body">
          {current ? (
            <>
              <div>
                <span className="agent-recipe__name">{current.name}</span>
                {current.description && (
                  <p className="agent-recipe__desc">{current.description}</p>
                )}
              </div>
              <pre className="agent-recipe__yaml">{current.yaml}</pre>
            </>
          ) : (
            <p className="muted-12">Select a recipe to view its YAML.</p>
          )}
        </CardContent>
      </Card>
    </div>
  );
}

