import { useEffect } from "react";
import type { CompareResponse, CompareSide } from "../types";
import { statusTone } from "../lib";
import { statusLabel } from "./RichText";

interface Props {
  comparison?: CompareResponse;
  comparing?: boolean;
  onSelectRuns?: (leftRun?: string, rightRun?: string) => void;
  onClose: () => void;
}

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms} ms`;
  return `${(ms / 1000).toFixed(2)} s`;
}

function Side({
  side,
  label,
  runs,
  disabled,
  onSelect,
}: {
  side: CompareSide;
  label: string;
  runs: CompareResponse["left_runs"];
  disabled: boolean;
  onSelect: (runId: string) => void;
}) {
  const metrics = side.metrics;
  const rows: Array<[string, string | number]> = [
    ["Steps", metrics.steps],
    ["Model calls", metrics.model_calls],
    ["Tool calls", metrics.tool_calls],
    ["Approvals", metrics.approvals],
    ["Fallbacks", metrics.fallbacks],
    ["Decisions", metrics.decisions],
    ["Observations", metrics.observations],
    ["Duration", formatDuration(metrics.duration_ms)],
  ];
  return (
    <div className="compare-col">
      <div className="compare-col-head">
        <div>
          <div className="eyebrow">{label}</div>
          <div className="compare-title">{side.session.title}</div>
          <div className="mono small">{side.run_id.slice(0, 16)}</div>
        </div>
        <span className={`pill pill-${statusTone(metrics.status)}`}>
          {statusLabel(metrics.status)}
        </span>
      </div>

      <select
        className="compare-run-select"
        value={side.run_id}
        disabled={disabled || runs.length === 0}
        onChange={(event) => onSelect(event.target.value)}
      >
        {runs.map((run) => (
          <option key={run.run_id} value={run.run_id}>
            {run.goal.slice(0, 42) || "untitled"} · {statusLabel(run.status)} ·{" "}
            {run.run_id.slice(-6)}
          </option>
        ))}
      </select>

      <table className="compare-table">
        <tbody>
          {rows.map(([name, value]) => (
            <tr key={name}>
              <th>{name}</th>
              <td>{value}</td>
            </tr>
          ))}
        </tbody>
      </table>

      <div className="compare-tools">
        {Object.entries(metrics.tools).map(([tool, count]) => (
          <span className="compare-tool" key={tool}>
            <code>{tool}</code>
            <span>{count}</span>
          </span>
        ))}
        {Object.keys(metrics.tools).length === 0 && (
          <span className="muted small">No tools executed.</span>
        )}
      </div>

      {metrics.files.length > 0 && (
        <div className="compare-files">
          {metrics.files.map((file) => (
            <div className="compare-file" key={file.path}>
              <code>{file.path}</code>
              <span className="diff-added">+{file.added}</span>
              <span className="diff-removed">-{file.removed}</span>
            </div>
          ))}
        </div>
      )}

      {metrics.final_answer && (
        <pre className="compare-answer">{metrics.final_answer}</pre>
      )}
    </div>
  );
}

export function ComparePanel({
  comparison,
  comparing = false,
  onSelectRuns,
  onClose,
}: Props) {
  useEffect(() => {
    if (!comparison) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [comparison, onClose]);

  if (!comparison) return null;
  return (
    <div
      className="compare-overlay"
      role="dialog"
      aria-modal="true"
      aria-label="Run comparison"
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="compare-panel">
        <header className="compare-header">
          <div>
            <h2>Run comparison</h2>
            <div className="muted small">
              Newest branch run vs its parent source run
            </div>
          </div>
          <button className="btn ghost small" onClick={onClose}>
            Close
          </button>
        </header>

        <div className="compare-grid">
          <Side
            side={comparison.left}
            label="Branch run"
            runs={comparison.left_runs}
            disabled={comparing}
            onSelect={(runId) => onSelectRuns?.(runId, comparison.right.run_id)}
          />
          <Side
            side={comparison.right}
            label="Parent source run"
            runs={comparison.right_runs}
            disabled={comparing}
            onSelect={(runId) => onSelectRuns?.(comparison.left.run_id, runId)}
          />
        </div>

        {(comparison.only_left.length > 0 ||
          comparison.only_right.length > 0 ||
          comparison.shared_files.length > 0) && (
          <section className="compare-file-sets">
            <h3>Files</h3>
            <div className="compare-grid">
              <div>
                <div className="eyebrow">Only branch</div>
                {comparison.only_left.map((path) => (
                  <code className="fact" key={path}>
                    {path}
                  </code>
                ))}
              </div>
              <div>
                <div className="eyebrow">Only parent</div>
                {comparison.only_right.map((path) => (
                  <code className="fact" key={path}>
                    {path}
                  </code>
                ))}
              </div>
            </div>
            {comparison.shared_files.length > 0 && (
              <div className="muted small">
                Shared: {comparison.shared_files.join(", ")}
              </div>
            )}
          </section>
        )}
      </div>
    </div>
  );
}
