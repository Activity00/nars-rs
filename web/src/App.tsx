import { useCallback, useEffect, useMemo, useReducer, useState } from "react";
import { api, eventStreamUrl, getToken, setToken } from "./api";
import { initialState, reducer } from "./store";
import type { SessionMeta, ToolManifest } from "./types";
import { Composer } from "./components/Composer";
import { Inspector } from "./components/Inspector";
import { Sidebar } from "./components/Sidebar";
import { Timeline } from "./components/Timeline";
import { statusLabel } from "./components/RichText";
import { statusTone } from "./lib";

export default function App() {
  const [state, dispatch] = useReducer(reducer, initialState);
  const [busyCallId, setBusyCallId] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [token, setTokenValue] = useState(getToken());

  const selectedId = state.selected?.sessionId;

  const refreshSessions = useCallback(async (): Promise<SessionMeta[]> => {
    const data = await api.sessions();
    const sessions = data.sessions as SessionMeta[];
    dispatch({ type: "sessions", sessions });
    return sessions;
  }, []);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const [health, sessions] = await Promise.all([api.health(), api.sessions()]);
        if (cancelled) return;
        dispatch({
          type: "tools",
          tools: health.tools as ToolManifest[],
          provider: health.provider,
          model: health.model,
        });
        const list = sessions.sessions as SessionMeta[];
        dispatch({ type: "sessions", sessions: list });
        if (list.length > 0) {
          dispatch({ type: "select", sessionId: list[0].session_id });
        }
      } catch (error) {
        if (!cancelled) {
          dispatch({ type: "error", message: String(error) });
          dispatch({ type: "loading", value: false });
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!selectedId) return;
    let source: EventSource | undefined;
    dispatch({ type: "connection", connection: "connecting" });
    source = new EventSource(eventStreamUrl(selectedId));
    source.onopen = () => dispatch({ type: "connection", connection: "live" });
    source.onerror = () => dispatch({ type: "connection", connection: "error" });
    source.addEventListener("agent", (event) => {
      try {
        const envelope = JSON.parse((event as MessageEvent).data);
        dispatch({ type: "event", envelope });
      } catch {
        // ignore malformed frames
      }
    });
    return () => source?.close();
  }, [selectedId]);

  const selectSession = useCallback(
    (id: string) => {
      if (id !== selectedId) dispatch({ type: "select", sessionId: id });
    },
    [selectedId],
  );

  const createSession = useCallback(
    async (workspace: string) => {
      try {
        const created = await api.createSession(workspace);
        await refreshSessions();
        dispatch({ type: "select", sessionId: created.session.session_id });
      } catch (error) {
        dispatch({ type: "error", message: String(error) });
      }
    },
    [refreshSessions],
  );

  const send = useCallback(
    async (text: string) => {
      if (!selectedId) return;
      try {
        await api.sendMessage(selectedId, text);
      } catch (error) {
        dispatch({ type: "error", message: String(error) });
        throw error;
      }
    },
    [selectedId],
  );

  const decide = useCallback(async (callId: string, approved: boolean) => {
    setBusyCallId(callId);
    try {
      await api.approve(callId, approved);
    } catch (error) {
      dispatch({ type: "error", message: String(error) });
    } finally {
      setBusyCallId(undefined);
    }
  }, []);

  const run = useMemo(() => {
    const selected = state.selected;
    if (!selected) return undefined;
    const id = selected.activeRunId ?? selected.runOrder[selected.runOrder.length - 1];
    return id ? selected.runs[id] : undefined;
  }, [state.selected]);

  const control = useCallback(
    async (action: "pause" | "resume" | "cancel") => {
      if (!run) return;
      setBusy(true);
      try {
        if (action === "pause") await api.pause(run.id);
        else if (action === "resume") await api.resume(run.id);
        else await api.cancel(run.id);
      } catch (error) {
        dispatch({ type: "error", message: String(error) });
      } finally {
        setBusy(false);
      }
    },
    [run],
  );

  const session = state.sessions.find((item: SessionMeta) => item.session_id === selectedId);

  return (
    <div className="app">
      <Sidebar
        sessions={state.sessions}
        selectedId={selectedId}
        tools={state.tools}
        connection={state.selected?.connection ?? "connecting"}
        provider={state.provider}
        model={state.model}
        token={token}
        onSelect={selectSession}
        onCreate={(workspace) => void createSession(workspace)}
        onSaveToken={(value) => {
          setToken(value);
          setTokenValue(value);
          if (selectedId) dispatch({ type: "select", sessionId: selectedId });
        }}
      />

      <main className="center">
        <header className="topbar">
          <div>
            <div className="topbar-title">{session?.title ?? "nars-agent"}</div>
            <div className="mono small">{session?.workspace ?? "no workspace selected"}</div>
          </div>
          {run && (
            <span className={`pill pill-${statusTone(run.status)}`}>
              {statusLabel(run.status)} · step {run.step}
            </span>
          )}
        </header>

        {state.globalError && (
          <div className="banner error">
            <span>{state.globalError}</span>
            <button className="btn ghost small" onClick={() => dispatch({ type: "error" })}>
              Dismiss
            </button>
          </div>
        )}

        <Timeline
          timeline={state.selected?.timeline ?? []}
          runs={state.selected?.runs ?? {}}
          busyCallId={busyCallId}
          onDecision={(callId, approved) => void decide(callId, approved)}
        />

        <Composer disabled={!selectedId} busy={busy} onSend={send} />
      </main>

      <Inspector
        run={run}
        busy={busy}
        onPause={() => void control("pause")}
        onResume={() => void control("resume")}
        onCancel={() => void control("cancel")}
      />
    </div>
  );
}
