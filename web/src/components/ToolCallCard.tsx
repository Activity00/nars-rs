import type { ToolCallView } from "../types";
import { diffsOf, formatDuration, prettyJson, statusTone } from "../lib";
import { DiffView } from "./DiffView";

interface Props {
  view: ToolCallView;
}

export function ToolCallCard({ view }: Props) {
  const { call, result, observation } = view;
  const status = result?.status ?? "running";
  return (
    <div className={`tool-card tool-${statusTone(status)}`}>
      <div className="tool-head">
        <span className="tool-name">{call.tool}</span>
        <span className={`pill pill-${statusTone(status)}`}>{status.replaceAll("_", " ")}</span>
        {result && <span className="muted small">{formatDuration(result.duration_ms)}</span>}
      </div>
      <details className="tool-args">
        <summary>arguments</summary>
        <pre>{prettyJson(call.arguments)}</pre>
      </details>
      {result?.error && <div className="tool-error">{result.error}</div>}
      {result?.stdout && (
        <pre className="tool-output" title="stdout">
          {result.stdout}
        </pre>
      )}
      {result?.stderr && (
        <pre className="tool-output tool-stderr" title="stderr">
          {result.stderr}
        </pre>
      )}
      {diffsOf(result).map((diff) => (
        <DiffView key={diff.path} diff={diff.unified} path={diff.path} />
      ))}
      {result?.truncated && <div className="muted small">output truncated</div>}
      {observation && (
        <div className="tool-observation">
          <span className={`pill pill-${observation.outcome === "success" ? "ok" : "warn"}`}>
            {observation.outcome}
          </span>
          <span>{observation.summary}</span>
        </div>
      )}
    </div>
  );
}
