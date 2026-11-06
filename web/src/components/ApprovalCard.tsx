import type { PendingApproval } from "../types";
import { DiffView } from "./DiffView";

interface Props {
  pending: PendingApproval;
  busy: boolean;
  onDecision: (approved: boolean) => void;
}

export function ApprovalCard({ pending, busy, onDecision }: Props) {
  return (
    <div className="approval-card">
      <div className="approval-head">
        <span className="pill pill-warn">approval required</span>
        <span className="tool-name">{pending.call.tool}</span>
      </div>
      <div className="approval-reason">{pending.reason}</div>
      {pending.preview?.summary && (
        <div className="muted small">
          {pending.preview.summary}
          {pending.preview.files.length > 0 && ` · ${pending.preview.files.join(", ")}`}
        </div>
      )}
      {pending.preview?.unified_diff && <DiffView diff={pending.preview.unified_diff} />}
      <div className="approval-actions">
        <button className="btn primary" disabled={busy} onClick={() => onDecision(true)}>
          Approve &amp; run
        </button>
        <button className="btn ghost" disabled={busy} onClick={() => onDecision(false)}>
          Deny
        </button>
      </div>
    </div>
  );
}
