import {
  ApiError,
  type AgentRecipe,
  type AgentTool,
  type ChatEvent,
  type Device,
  type HealthResponse,
  type MemoryFragment,
  type ModelEntry,
  type ModelMemoryStatus,
  type PromptExtra,
  type PromptTemplate,
  type Schedule,
  type Settings,
  type TranscribeResponse,
  type UserSkill,
} from "./types";

// ────────────────────────────────────────────────────────────
// PondApiClient — single API port for the pond-desktop app.
// All REST calls MUST go through this class. No fetch() elsewhere.
// ────────────────────────────────────────────────────────────

declare global {
  interface Window {
    __GIAP_SERVER_URL__?: string;
  }
}

export class PondApiClient {
  private readonly base: string;
  private token: string | null;

  constructor(base?: string, token?: string | null) {
    this.base = (base ?? window.__GIAP_SERVER_URL__ ?? "http://127.0.0.1:4000").replace(/\/$/, "");
    this.token = token ?? null;
  }

  setToken(token: string | null): void {
    this.token = token;
  }

  // ── Internal helpers ───────────────────────────────────────

  private headers(extra?: Record<string, string>): Record<string, string> {
    const h: Record<string, string> = { "Content-Type": "application/json", ...extra };
    if (this.token) h["Authorization"] = `Bearer ${this.token}`;
    return h;
  }

  private async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const res = await fetch(`${this.base}${path}`, {
      method,
      headers: this.headers(),
      body: body !== undefined ? JSON.stringify(body) : undefined,
    });
    if (!res.ok) {
      let msg = res.statusText;
      try { msg = (await res.json()).message ?? msg; } catch { /* ignore */ }
      throw new ApiError(res.status, msg);
    }
    return res.json() as Promise<T>;
  }

  private get<T>(path: string): Promise<T>                  { return this.request<T>("GET", path); }
  private post<T>(path: string, body?: unknown): Promise<T>  { return this.request<T>("POST", path, body); }
  private put<T>(path: string, body?: unknown): Promise<T>   { return this.request<T>("PUT", path, body); }
  private del<T = void>(path: string): Promise<T>            { return this.request<T>("DELETE", path); }

  // ── Health ────────────────────────────────────────────────

  health(): Promise<HealthResponse> {
    return this.get("/api/v1/health");
  }

  // ── Settings ──────────────────────────────────────────────

  getSettings(): Promise<Settings> {
    return this.get("/api/v1/settings");
  }

  updateSettings(patch: Partial<Settings>): Promise<Settings> {
    return this.put("/api/v1/settings", patch);
  }

  // ── Devices ───────────────────────────────────────────────

  listDevices(): Promise<Device[]> {
    return this.get("/api/v1/devices");
  }

  // ── Schedules ─────────────────────────────────────────────

  listSchedules(): Promise<Schedule[]> {
    return this.get("/api/v1/schedules");
  }

  createSchedule(body: Omit<Schedule, "id" | "created_at">): Promise<Schedule> {
    return this.post("/api/v1/schedules", body);
  }

  deleteSchedule(id: string): Promise<void> {
    return this.del(`/api/v1/schedules/${id}`);
  }

  // ── Memory ────────────────────────────────────────────────

  listMemories(limit = 20): Promise<MemoryFragment[]> {
    return this.get(`/api/v1/memories?limit=${limit}`);
  }

  addMemory(content: string, tags?: string[]): Promise<MemoryFragment> {
    return this.post("/api/v1/memories", { content, tags });
  }

  deleteMemory(id: string): Promise<void> {
    return this.del(`/api/v1/memories/${id}`);
  }

  // ── Skills ────────────────────────────────────────────────

  listSkills(all = false): Promise<UserSkill[]> {
    return this.get(`/api/v1/skills${all ? "?all=true" : ""}`);
  }

  addSkill(name: string, content: string): Promise<UserSkill> {
    return this.post("/api/v1/skills", { name, content });
  }

  toggleSkill(id: string): Promise<UserSkill> {
    return this.put(`/api/v1/skills/${id}/toggle`);
  }

  removeSkill(id: string): Promise<void> {
    return this.del(`/api/v1/skills/${id}`);
  }

  // ── Models ────────────────────────────────────────────────

  listModels(): Promise<ModelEntry[]> {
    return this.get("/api/v1/models");
  }

  getMemoryStatus(): Promise<ModelMemoryStatus> {
    return this.get("/api/v1/models/memory-status");
  }

  // ── Prompts ───────────────────────────────────────────────

  listPrompts(): Promise<PromptTemplate[]> {
    return this.get("/api/v1/prompts");
  }

  getPrompt(name: string): Promise<PromptTemplate> {
    return this.get(`/api/v1/prompts/${name}`);
  }

  updatePrompt(name: string, content: string): Promise<PromptTemplate> {
    return this.put(`/api/v1/prompts/${name}`, { content });
  }

  resetPrompt(name: string): Promise<PromptTemplate> {
    return this.post(`/api/v1/prompts/${name}/reset`);
  }

  // ── Agent extras ──────────────────────────────────────────

  listExtras(): Promise<PromptExtra[]> {
    return this.get("/api/v1/agent/extras");
  }

  addExtra(key: string, content: string): Promise<PromptExtra> {
    return this.post("/api/v1/agent/extras", { key, content });
  }

  deleteExtra(key: string): Promise<void> {
    return this.del(`/api/v1/agent/extras/${encodeURIComponent(key)}`);
  }

  listTools(): Promise<AgentTool[]> {
    return this.get("/api/v1/agent/tools");
  }

  // ── Recipes ───────────────────────────────────────────────

  listRecipes(): Promise<AgentRecipe[]> {
    return this.get("/api/v1/recipes");
  }

  getRecipe(name: string): Promise<AgentRecipe> {
    return this.get(`/api/v1/recipes/${name}`);
  }

  // ── Chat streaming ────────────────────────────────────────
  //
  // Returns an AsyncGenerator that yields ChatEvent objects.
  // Usage:
  //   for await (const event of client.chatStream("hello")) {
  //     if (event.type === "text") appendToken(event.content ?? "");
  //   }

  async *chatStream(
    message: string,
    sessionId?: string,
    token?: string,
  ): AsyncGenerator<ChatEvent> {
    const headers: Record<string, string> = { "Content-Type": "application/json" };
    const tok = token ?? this.token;
    if (tok) headers["Authorization"] = `Bearer ${tok}`;

    const res = await fetch(`${this.base}/api/v1/chat/stream`, {
      method: "POST",
      headers,
      body: JSON.stringify({ message, session_id: sessionId }),
    });

    if (!res.ok) {
      let msg = res.statusText;
      try { msg = (await res.json()).message ?? msg; } catch { /* ignore */ }
      throw new ApiError(res.status, msg);
    }

    const reader = res.body!.getReader();
    const decoder = new TextDecoder();
    let buffer = "";

    try {
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;

        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split("\n");
        buffer = lines.pop() ?? "";

        for (const line of lines) {
          const trimmed = line.trim();
          if (!trimmed || trimmed === "data: [DONE]") {
            if (trimmed === "data: [DONE]") yield { type: "done", done: true };
            continue;
          }
          const data = trimmed.startsWith("data: ") ? trimmed.slice(6) : trimmed;
          try {
            const event = JSON.parse(data) as ChatEvent;
            yield event;
          } catch {
            // Malformed SSE line — skip
          }
        }
      }
    } finally {
      reader.releaseLock();
    }
  }

  // ── Transcription ─────────────────────────────────────────

  async transcribe(wav: ArrayBuffer, token?: string): Promise<TranscribeResponse> {
    const headers: Record<string, string> = {};
    const tok = token ?? this.token;
    if (tok) headers["Authorization"] = `Bearer ${tok}`;

    const form = new FormData();
    form.append("file", new Blob([wav], { type: "audio/wav" }), "audio.wav");

    const res = await fetch(`${this.base}/api/v1/transcribe`, {
      method: "POST",
      headers,
      body: form,
    });

    if (!res.ok) {
      let msg = res.statusText;
      try { msg = (await res.json()).message ?? msg; } catch { /* ignore */ }
      throw new ApiError(res.status, msg);
    }

    return res.json() as Promise<TranscribeResponse>;
  }
}

// Singleton — the Tauri backend injects window.__GIAP_SERVER_URL__
export const api = new PondApiClient();
