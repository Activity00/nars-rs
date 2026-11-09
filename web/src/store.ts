import type {
  AppState,
  NarsDecision,
  RunSummary,
  RunView,
  SelectedSessionState,
  SessionMeta,
  TimelineItem,
  ToolCallView,
  ToolManifest,
  WireEnvelope,
} from "./types";

export type Action =
  | { type: "sessions"; sessions: SessionMeta[] }
  | { type: "select"; sessionId: string }
  | { type: "summaries"; summaries: RunSummary[] }
  | { type: "select-run"; runId?: string }
  | { type: "tools"; tools: ToolManifest[]; provider?: string; model?: string }
  | { type: "event"; envelope: WireEnvelope }
  | { type: "connection"; connection: SelectedSessionState["connection"] }
  | { type: "error"; message?: string }
  | { type: "loading"; value: boolean };

export const initialState: AppState = {
  sessions: [],
  tools: [],
  loadingSessions: true,
};

export function reducer(state: AppState, action: Action): AppState {
  switch (action.type) {
    case "sessions":
      return { ...state, sessions: action.sessions, loadingSessions: false };
    case "select":
      return {
        ...state,
        selected: {
          sessionId: action.sessionId,
          timeline: [],
          runs: {},
          runOrder: [],
          summaries: [],
          lastSeq: 0,
          connection: "connecting",
        },
      };
    case "summaries": {
      if (!state.selected) return state;
      const summaries = action.summaries;
      const ids = summaries.map((summary) => summary.run_id);
      const runOrder = [
        ...ids,
        ...state.selected.runOrder.filter((id) => !ids.includes(id)),
      ];
      return {
        ...state,
        selected: {
          ...state.selected,
          summaries,
          runOrder,
          activeRunId: state.selected.activeRunId ?? ids[0],
        },
      };
    }
    case "select-run":
      return state.selected
        ? {
            ...state,
            selected: { ...state.selected, activeRunId: action.runId },
          }
        : state;
    case "tools":
      return {
        ...state,
        tools: action.tools,
        provider: action.provider ?? state.provider,
        model: action.model ?? state.model,
      };
    case "connection":
      return state.selected
        ? { ...state, selected: { ...state.selected, connection: action.connection } }
        : state;
    case "error":
      return { ...state, globalError: action.message };
    case "loading":
      return { ...state, loadingSessions: action.value };
    case "event":
      return applyEnvelope(state, action.envelope);
    default:
      return state;
  }
}

function emptyRun(id: string, sessionId: string, task: unknown, ts: number): RunView {
  return {
    id,
    sessionId,
    status: "planning",
    step: 0,
    task: task as RunView["task"],
    facts: [],
    decisions: [],
    observations: [],
    toolCalls: {},
    usage: { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 },
    createdAt: ts,
  };
}

export function applyEnvelope(state: AppState, envelope: WireEnvelope): AppState {
  const selected = state.selected;
  if (!selected) return state;
  if (envelope.seq <= selected.lastSeq) return state;

  const event = envelope.event;
  const ts = envelope.timestamp_ms;
  const next: SelectedSessionState = {
    ...selected,
    timeline: [...selected.timeline],
    runs: { ...selected.runs },
    runOrder: [...selected.runOrder],
    lastSeq: envelope.seq,
  };
  const runId = typeof event.run_id === "string" ? event.run_id : undefined;
  const id = `${event.type}-${envelope.seq}`;

  switch (event.type) {
    case "user_message":
      push(next, {
        kind: "user",
        id,
        text: String(event.text ?? ""),
        runId,
        ts,
      });
      break;
    case "run_started":
      if (runId) {
        next.runs[runId] = emptyRun(runId, selected.sessionId, event.task, ts);
        if (!next.runOrder.includes(runId)) next.runOrder.push(runId);
        next.activeRunId = runId;
      }
      break;
    case "model_requested":
      if (runId)
        updateRun(next, runId, (run) => {
          run.modelPurpose = String(event.purpose ?? "plan");
          if (run.status !== "paused") run.status = "planning";
        });
      break;
    case "model_proposed":
      if (runId)
        updateRun(next, runId, (run) => {
          run.modelPurpose = undefined;
          const usage = event.usage;
          if (usage) {
            run.usage = {
              prompt_tokens: run.usage.prompt_tokens + (usage.prompt_tokens ?? 0),
              completion_tokens:
                run.usage.completion_tokens + (usage.completion_tokens ?? 0),
              total_tokens: run.usage.total_tokens + (usage.total_tokens ?? 0),
            };
          }
        });
      break;
    case "model_failed":
      if (runId) {
        updateRun(next, runId, (run) => {
          run.modelPurpose = undefined;
        });
        push(next, {
          kind: "status",
          id,
          runId,
          text: `Model issue: ${String(event.error ?? "unknown error")}`,
          tone: "error",
          ts,
        });
      }
      break;
    case "nars_goal_committed":
      if (runId)
        updateRun(next, runId, (run) => {
          run.goal = event.goal;
          run.facts = [...run.facts, ...(event.facts ?? [])].slice(-64);
        });
      break;
    case "nars_decision_made":
      if (runId) {
        const decision = event.decision as NarsDecision;
        updateRun(next, runId, (run) => {
          run.step = Math.max(run.step, decision.step ?? 0);
          run.decisions = [...run.decisions, decision].slice(-40);
          if (decision.plan) run.plan = decision.plan;
        });
        push(next, { kind: "decision", id, runId, decision, ts });
      }
      break;
    case "approval_requested":
      if (runId) {
        const call = event.call;
        updateRun(next, runId, (run) => {
          run.status = "awaiting_approval";
          run.pendingApproval = {
            call,
            reason: String(event.reason ?? ""),
            preview: event.preview,
          };
        });
        push(next, { kind: "approval", id, runId, callId: call.id, ts });
      }
      break;
    case "approval_resolved":
      if (runId) {
        updateRun(next, runId, (run) => {
          run.pendingApproval = undefined;
          run.status = event.approved ? "executing_tool" : "planning";
        });
        push(next, {
          kind: "status",
          id,
          runId,
          text: event.approved ? "Patch approved" : "Patch denied",
          tone: event.approved ? "success" : "info",
          ts,
        });
      }
      break;
    case "tool_started": {
      if (runId) {
        const call = event.call;
        updateRun(next, runId, (run) => {
          run.status = "executing_tool";
          run.toolCalls = {
            ...run.toolCalls,
            [call.id]: { call } satisfies ToolCallView,
          };
        });
        push(next, { kind: "tool", id, runId, callId: call.id, ts });
      }
      break;
    }
    case "tool_finished":
      if (runId) {
        const result = event.result;
        updateRun(next, runId, (run) => {
          const existing = run.toolCalls[result.call_id];
          if (existing) {
            run.toolCalls = {
              ...run.toolCalls,
              [result.call_id]: { ...existing, result },
            };
          }
        });
      }
      break;
    case "observation_recorded":
      if (runId) {
        const observation = event.observation;
        updateRun(next, runId, (run) => {
          run.observations = [...run.observations, observation].slice(-40);
          if (event.plan) run.plan = event.plan;
          const existing = run.toolCalls[observation.call_id];
          if (existing) {
            run.toolCalls = {
              ...run.toolCalls,
              [observation.call_id]: { ...existing, observation },
            };
          }
        });
      }
      break;
    case "assistant_message":
      if (runId) {
        push(next, {
          kind: "assistant",
          id,
          text: String(event.text ?? ""),
          runId,
          ts,
        });
      }
      break;
    case "run_paused":
      if (runId)
        updateRun(next, runId, (run) => {
          run.status = "paused";
          run.modelPurpose = undefined;
        });
      push(next, {
        kind: "status",
        id,
        runId,
        text: `Paused: ${String(event.reason ?? "")}`,
        tone: "info",
        ts,
      });
      break;
    case "run_resumed":
      if (runId)
        updateRun(next, runId, (run) => {
          run.status = "planning";
        });
      push(next, { kind: "status", id, runId, text: "Resumed", tone: "info", ts });
      break;
    case "run_completed":
      if (runId)
        updateRun(next, runId, (run) => {
          run.status = "completed";
          run.modelPurpose = undefined;
        });
      push(next, {
        kind: "status",
        id,
        runId,
        text: event.success ? "Run completed" : "Run ended",
        tone: event.success ? "success" : "info",
        ts,
      });
      break;
    case "run_failed":
      if (runId)
        updateRun(next, runId, (run) => {
          run.status = "failed";
          run.error = { message: String(event.error?.message ?? "run failed") };
          run.modelPurpose = undefined;
        });
      push(next, {
        kind: "status",
        id,
        runId,
        text: `Run failed: ${String(event.error?.message ?? "")}`,
        tone: "error",
        ts,
      });
      break;
    case "run_cancelled":
      if (runId)
        updateRun(next, runId, (run) => {
          run.status = "cancelled";
          run.modelPurpose = undefined;
        });
      push(next, { kind: "status", id, runId, text: "Run cancelled", tone: "error", ts });
      break;
    default:
      break;
  }

  return { ...state, selected: next };
}

function updateRun(
  selected: SelectedSessionState,
  runId: string,
  update: (run: RunView) => void,
): void {
  const existing = selected.runs[runId];
  if (!existing) return;
  const run: RunView = {
    ...existing,
    facts: [...existing.facts],
    decisions: [...existing.decisions],
    observations: [...existing.observations],
    toolCalls: { ...existing.toolCalls },
    usage: { ...existing.usage },
  };
  update(run);
  selected.runs[runId] = run;
}

function push(selected: SelectedSessionState, item: TimelineItem): void {
  selected.timeline.push(item);
}
