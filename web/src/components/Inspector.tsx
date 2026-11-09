import type { RunView, SessionMeta } from "../types";
import { diffsOf, formatTokens, statusTone } from "../lib";
import { statusLabel } from "./RichText";

interface Props {
  run?: RunView;
  busy: boolean;
  onPause: () => void;
  onResume: () => void;
  onCancel: () => void;
  onBranch?: () => void;
  parent?: SessionMeta;
  branchSource?: string;
  childCount?: number;
  onOpenParent?: () => void;
}

export function Inspector({
  run,
  busy,
  onPause,
  onResume,
  onCancel,
  onBranch,
  parent,
  branchSource,
  childCount = 0,
  onOpenParent,
}: Props) {
  if (!run) {
    return (
      <aside className="inspector">
        <h2>Agent state</h2>
        <p className="muted">Select or start a run to inspect NARS goals, decisions, and tools.</p>
      </aside>
    );
  }

  const decision = run.decisions[run.decisions.length - 1];
  const files = new Map<string, { added: number; removed: number }>();
  for (const view of Object.values(run.toolCalls)) {
    for (const diff of diffsOf(view.result)) {
      const current = files.get(diff.path) ?? { added: 0, removed: 0 };
      files.set(diff.path, {
        added: current.added + diff.added,
        removed: current.removed + diff.removed,
      });
    }
  }

  const active = ["planning", "executing_tool", "awaiting_approval", "pending"].includes(
    run.status,
  );
  const terminal = ["completed", "failed", "cancelled"].includes(run.status);

  return (
    <aside className="inspector">
      <div className="inspector-head">
        <h2>Agent state</h2>
        <span className={`pill pill-${statusTone(run.status)}`}>{statusLabel(run.status)}</span>
      </div>

      <section>
        <h3>Goal</h3>
        <div className="goal-card">
          <div className="goal-type">{run.task?.goal.type ?? "task"}</div>
          <div className="goal-text">{run.task?.goal.description ?? "—"}</div>
          {run.task?.goal.target && (
            <div className="mono small">{run.task.goal.target}</div>
          )}
          {run.task?.constraints && run.task.constraints.length > 0 && (
            <ul className="constraints">
              {run.task.constraints.map((constraint) => (
                <li key={constraint}>{constraint}</li>
              ))}
            </ul>
          )}
        </div>
      </section>

      {(parent || childCount > 0) && (
        <section>
          <h3>Lineage</h3>
          <div className="lineage-card">
            {parent ? (
              <button
                className="lineage-link"
                onClick={onOpenParent}
                title={branchSource ? `Source run ${branchSource}` : "Parent session"}
              >
                <span>↳ {parent.title}</span>
                {branchSource && (
                  <span className="mono small">{branchSource.slice(0, 16)}</span>
                )}
              </button>
            ) : (
              <div className="muted small">Root session</div>
            )}
            {childCount > 0 && (
              <div className="muted small">
                {childCount} branch session{childCount === 1 ? "" : "s"}
              </div>
            )}
          </div>
        </section>
      )}

      {run.plan && run.plan.stages.length > 0 && (
        <section>
          <h3>Task plan</h3>
          <ol className="plan">
            {run.plan.stages.map((stage) => (
              <li className={`plan-stage plan-${stage.status}`} key={stage.id}>
                <span className="plan-dot" />
                <span className="plan-label">{stage.label}</span>
                <span className="muted small">
                  {stage.status}
                  {stage.overdue ? " · overdue" : ""}
                </span>
              </li>
            ))}
          </ol>
          {run.plan.notes.length > 0 && (
            <div className="plan-notes">{run.plan.notes.slice(-3).join(" · ")}</div>
          )}
          {run.plan.working_memory.length > 0 && (
            <div className="facts">
              {run.plan.working_memory.map((file) => (
                <code className="fact" key={file}>
                  {file}
                </code>
              ))}
            </div>
          )}
        </section>
      )}

      <section>
        <h3>NARS</h3>
        <div className="kv">
          <span>goal atom</span>
          <code>{run.goal?.term ?? "—"}</code>
        </div>
        <div className="kv">
          <span>priority</span>
          <span>{run.goal ? run.goal.priority.toFixed(2) : "—"}</span>
        </div>
        <div className="kv">
          <span>step</span>
          <span>
            {run.step} / {run.task?.max_steps ?? "?"}
          </span>
        </div>
        <div className="kv">
          <span>activity</span>
          <span>{run.modelPurpose ?? "idle"}</span>
        </div>
        <div className="facts">
          {run.facts.slice(-12).map((fact) => (
            <code className="fact" key={fact.term} title={`${fact.truth.frequency} ${fact.truth.confidence}`}>
              {fact.term}
            </code>
          ))}
          {run.facts.length === 0 && <span className="muted small">no facts committed yet</span>}
        </div>
      </section>

      {decision && (
        <section>
          <h3>Last decision</h3>
          <div className="decision-rationale">{decision.rationale}</div>
          <div className="candidates">
            {decision.scored.map((candidate) => (
              <div
                className={`candidate ${
                  candidate.candidate_id === decision.selected?.candidate_id
                    ? "candidate-selected"
                    : ""
                }`}
                key={candidate.candidate_id}
              >
                <span className="candidate-tool">{candidate.tool}</span>
                <span className="score-bar">
                  <span style={{ width: `${Math.round(candidate.score * 100)}%` }} />
                </span>
                <span className="muted small">{candidate.score.toFixed(2)}</span>
              </div>
            ))}
          </div>
        </section>
      )}

      <section>
        <h3>Files changed</h3>
        {files.size === 0 ? (
          <span className="muted small">none yet</span>
        ) : (
          <ul className="file-list">
            {[...files.entries()].map(([path, stats]) => (
              <li key={path}>
                <span className="mono">{path}</span>
                <span className="small">
                  <span className="add">+{stats.added}</span>{" "}
                  <span className="del">-{stats.removed}</span>
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section>
        <h3>Usage</h3>
        <div className="kv">
          <span>tokens</span>
          <span>
            {formatTokens(run.usage.total_tokens)} ({formatTokens(run.usage.prompt_tokens)} in /{" "}
            {formatTokens(run.usage.completion_tokens)} out)
          </span>
        </div>
      </section>

      {run.error && (
        <section>
          <h3>Error</h3>
          <div className="tool-error">{run.error.message}</div>
        </section>
      )}

      <section className="controls">
        {active && (
          <button className="btn ghost" disabled={busy} onClick={onPause}>
            Pause
          </button>
        )}
        {run.status === "paused" && (
          <button className="btn primary" disabled={busy} onClick={onResume}>
            Resume
          </button>
        )}
        {terminal && onBranch && (
          <button className="btn ghost" disabled={busy} onClick={onBranch}>
            Branch &amp; retry
          </button>
        )}
        {active || run.status === "paused" ? (
          <button className="btn danger" disabled={busy} onClick={onCancel}>
            Cancel
          </button>
        ) : null}
      </section>
    </aside>
  );
}
