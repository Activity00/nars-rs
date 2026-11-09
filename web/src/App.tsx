import { useCallback, useEffect, useMemo, useReducer, useState } from "react";
import { api, eventStreamUrl, getToken, setToken } from "./api";
import { initialState, reducer } from "./store";
import type {
  CompareResponse,
  RunSummary,
  SessionMeta,
  ToolManifest,
} from "./types";
import { ComparePanel } from "./components/ComparePanel";
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
  const [comparing, setComparing] = useState(false);
  const [comparison, setComparison] = useState<CompareResponse>();
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
    setComparison(undefined);
  }, [selectedId]);

  useEffect(() => {
    if (!selectedId) return;
    let cancelled = false;
    void (async () => {
      try {
        const detail = await api.session(selectedId);
        if (!cancelled) {
          dispatch({
            type: "summaries",
            summaries: detail.runs as RunSummary[],
          });
        }
      } catch (error) {
        if (!cancelled) dispatch({ type: "error", message: String(error) });
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [selectedId]);

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

  const renameSession = useCallback(
    async (target: SessionMeta) => {
      const title = window.prompt("Session title", target.title);
      if (!title?.trim()) return;
      try {
        await api.renameSession(target.session_id, title.trim());
        await refreshSessions();
      } catch (error) {
        dispatch({ type: "error", message: String(error) });
      }
    },
    [refreshSessions],
  );

  const deleteSession = useCallback(
    async (target: SessionMeta) => {
      if (!window.confirm(`Delete "${target.title}" and all of its runs?`)) return;
      try {
        await api.deleteSession(target.session_id);
        const sessions = await refreshSessions();
        if (target.session_id === selectedId) {
          if (sessions.length > 0) {
            dispatch({ type: "select", sessionId: sessions[0].session_id });
          } else {
            dispatch({ type: "clear" });
          }
        }
      } catch (error) {
        dispatch({ type: "error", message: String(error) });
      }
    },
    [refreshSessions, selectedId],
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
    if (!selected?.activeRunId) return undefined;
    return selected.runs[selected.activeRunId];
  }, [state.selected]);

  const runOptions = useMemo(() => {
    const selected = state.selected;
    if (!selected) return [];
    const summaries = new Map(
      selected.summaries.map((summary) => [summary.run_id, summary]),
    );
    return selected.runOrder.map((id) => {
      const summary = summaries.get(id);
      const view = selected.runs[id];
      return {
        id,
        label: view?.task?.goal.description || summary?.goal || id,
        status: view?.status ?? summary?.status ?? "unknown",
      };
    });
  }, [state.selected]);

  const visibleTimeline = useMemo(() => {
    const timeline = state.selected?.timeline ?? [];
    const active = state.selected?.activeRunId;
    return active ? timeline.filter((item) => item.runId === active) : timeline;
  }, [state.selected]);

  const branch = useCallback(async () => {
    if (!selectedId || !run) return;
    setBusy(true);
    try {
      const result = await api.branch(selectedId, run.id);
      await refreshSessions();
      dispatch({ type: "select", sessionId: result.session.session_id });
    } catch (error) {
      dispatch({ type: "error", message: String(error) });
    } finally {
      setBusy(false);
    }
  }, [selectedId, run, refreshSessions]);

  const compare = useCallback(async () => {
    const selected = state.sessions.find(
      (item: SessionMeta) => item.session_id === selectedId,
    );
    if (!selectedId || !selected?.parent_session_id) return;
    setComparing(true);
    try {
      const result = await api.compare(selectedId, selected.parent_session_id);
      setComparison(result);
    } catch (error) {
      dispatch({ type: "error", message: String(error) });
    } finally {
      setComparing(false);
    }
  }, [selectedId, state.sessions]);

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
  const parentSession = session?.parent_session_id
    ? state.sessions.find(
        (item: SessionMeta) => item.session_id === session.parent_session_id,
      )
    : undefined;
  const branchChildren = selectedId
    ? state.sessions.filter(
        (item: SessionMeta) => item.parent_session_id === selectedId,
      ).length
    : 0;

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
        onRename={(session) => void renameSession(session)}
        onDelete={(session) => void deleteSession(session)}
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
            {session?.parent_session_id && parentSession && (
              <button
                className="lineage-link topbar-lineage"
                onClick={() => selectSession(session.parent_session_id!)}
                title={
                  session.branched_from_run_id
                    ? `Source run ${session.branched_from_run_id}`
                    : "Parent session"
                }
              >
                ↳ branch of {parentSession?.title ?? "parent session"}
              </button>
            )}
          </div>
          <div className="topbar-actions">
            {runOptions.length > 0 && (
              <label className="run-picker">
                <span>Run</span>
                <select
                  value={state.selected?.activeRunId ?? "all"}
                  onChange={(event) =>
                    dispatch({
                      type: "select-run",
                      runId:
                        event.target.value === "all"
                          ? undefined
                          : event.target.value,
                    })
                  }
                >
                  <option value="all">All runs</option>
                  {runOptions.map((option) => (
                    <option key={option.id} value={option.id}>
                      {option.label.slice(0, 72)} · {statusLabel(option.status)}
                    </option>
                  ))}
                </select>
              </label>
            )}
            {run && (
              <span className={`pill pill-${statusTone(run.status)}`}>
                {statusLabel(run.status)} · step {run.step}
              </span>
            )}
          </div>
        </header>

        {(state.provider === "demo" || state.provider === "echo") && (
          <div className="banner info">
            <span>
              <strong>Offline demo mode</strong> — no real model is configured. The server is
              using the deterministic <code>{state.provider}</code> provider (read-only
              investigation only). Set <code>NARS_AGENT_PROVIDER=openai</code>,{" "}
              <code>anthropic</code>, or <code>ollama</code> and restart <code>narsd</code> for
              real coding work.
            </span>
          </div>
        )}

        {state.globalError && (
          <div className="banner error">
            <span>{state.globalError}</span>
            <button className="btn ghost small" onClick={() => dispatch({ type: "error" })}>
              Dismiss
            </button>
          </div>
        )}

        <Timeline
          timeline={visibleTimeline}
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
        onBranch={() => void branch()}
        onCompare={parentSession ? () => void compare() : undefined}
        comparing={comparing}
        parent={parentSession}
        branchSource={session?.branched_from_run_id ?? undefined}
        childCount={branchChildren}
        onOpenParent={
          session?.parent_session_id
            ? () => selectSession(session.parent_session_id!)
            : undefined
        }
      />

      <ComparePanel
        comparison={comparison}
        onClose={() => setComparison(undefined)}
      />
    </div>
  );
}
