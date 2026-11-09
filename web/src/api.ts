import type { RunSummary } from "./types";

const TOKEN_KEY = "nars-agent-token";

export function getToken(): string {
  return localStorage.getItem(TOKEN_KEY) ?? "";
}

export function setToken(token: string): void {
  if (token) localStorage.setItem(TOKEN_KEY, token);
  else localStorage.removeItem(TOKEN_KEY);
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const token = getToken();
  const headers = new Headers(init?.headers);
  if (init?.body && !headers.has("content-type")) {
    headers.set("content-type", "application/json");
  }
  if (token) headers.set("authorization", `Bearer ${token}`);
  const response = await fetch(path, { ...init, headers });
  if (!response.ok) {
    let message = `${response.status} ${response.statusText}`;
    try {
      const body = await response.json();
      message = body?.error?.message ?? message;
    } catch {
      // keep the status text
    }
    throw new Error(message);
  }
  if (response.status === 204) return undefined as T;
  return (await response.json()) as T;
}

export interface SessionDetail {
  session: {
    session_id: string;
    workspace: string;
    title: string;
    created_at_ms: number;
    updated_at_ms: number;
  };
  runs: RunSummary[];
}

export const api = {
  health: () =>
    request<{ status: string; provider: string; model: string; tools: unknown[] }>(
      "/api/health",
    ),
  tools: () => request<{ tools: unknown[] }>("/api/tools"),
  sessions: () => request<{ sessions: any[] }>("/api/sessions"),
  session: (id: string) => request<SessionDetail>(`/api/sessions/${id}`),
  createSession: (workspace: string, title?: string) =>
    request<{ session: any }>("/api/sessions", {
      method: "POST",
      body: JSON.stringify({ workspace, title }),
    }),
  branch: (sessionId: string, runId: string) =>
    request<{ session: any; run_id: string; copied_events: number }>(
      `/api/sessions/${sessionId}/branch`,
      { method: "POST", body: JSON.stringify({ run_id: runId }) },
    ),
  sendMessage: (sessionId: string, text: string) =>
    request<{ run_id: string; session_id: string; status: string }>(
      `/api/sessions/${sessionId}/messages`,
      { method: "POST", body: JSON.stringify({ text }) },
    ),
  run: (runId: string) => request<{ run: any }>(`/api/runs/${runId}`),
  approve: (callId: string, approved: boolean, note?: string) =>
    request<{ ok: boolean }>(`/api/tool-calls/${callId}/approve`, {
      method: "POST",
      body: JSON.stringify({ approved, note }),
    }),
  pause: (runId: string) =>
    request<{ ok: boolean }>(`/api/runs/${runId}/pause`, { method: "POST" }),
  resume: (runId: string) =>
    request<{ ok: boolean }>(`/api/runs/${runId}/resume`, { method: "POST" }),
  cancel: (runId: string) =>
    request<{ ok: boolean }>(`/api/runs/${runId}/cancel`, { method: "POST" }),
};

export function eventStreamUrl(sessionId: string): string {
  const token = getToken();
  const query = token ? `?token=${encodeURIComponent(token)}` : "";
  return `/api/sessions/${sessionId}/events${query}`;
}
