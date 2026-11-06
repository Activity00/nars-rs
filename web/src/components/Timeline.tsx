import { useEffect, useRef } from "react";
import type { RunView, TimelineItem } from "../types";
import { ApprovalCard } from "./ApprovalCard";
import { RichText, formatTime } from "./RichText";
import { ToolCallCard } from "./ToolCallCard";

interface Props {
  timeline: TimelineItem[];
  runs: Record<string, RunView>;
  busyCallId?: string;
  onDecision: (callId: string, approved: boolean) => void;
}

export function Timeline({ timeline, runs, busyCallId, onDecision }: Props) {
  const bottomRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth", block: "end" });
  }, [timeline.length]);

  if (timeline.length === 0) {
    return (
      <div className="timeline empty">
        <div className="empty-card">
          <h2>NARS is the scheduler</h2>
          <p>
            Describe a task. The model proposes candidate actions, NARS chooses one, the tool
            broker executes it, and every step lands here as an auditable event.
          </p>
          <ul>
            <li>Read/search tools run automatically.</li>
            <li>Patches pause for a diff review.</li>
            <li>Pause, resume, cancel, or replay at any time.</li>
          </ul>
        </div>
      </div>
    );
  }

  return (
    <div className="timeline">
      {timeline.map((item) => {
        switch (item.kind) {
          case "user":
            return (
              <div className="event user-message" key={item.id}>
                <div className="event-meta">
                  <span>you</span>
                  <time>{formatTime(item.ts)}</time>
                </div>
                <RichText text={item.text} />
              </div>
            );
          case "assistant":
            return (
              <div className="event assistant-message" key={item.id}>
                <div className="event-meta">
                  <span>nars-agent</span>
                  <time>{formatTime(item.ts)}</time>
                </div>
                <RichText text={item.text} />
              </div>
            );
          case "decision": {
            const decision = item.decision;
            return (
              <div className="event decision" key={item.id}>
                <div className="decision-head">
                  <span className="eyebrow">NARS decision · step {decision.step}</span>
                  <span className={`pill pill-${decision.source === "nars" ? "ok" : "warn"}`}>
                    {decision.source}
                  </span>
                </div>
                <div className="decision-title">
                  {decision.selected ? (
                    <>
                      selected <strong>{decision.selected.tool}</strong>
                      <span className="muted small">
                        {" "}
                        score {decision.selected.score.toFixed(2)}
                      </span>
                    </>
                  ) : (
                    "no action selected"
                  )}
                </div>
                <div className="decision-rationale">{decision.rationale}</div>
                {decision.scored.length > 0 && (
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
                )}
              </div>
            );
          }
          case "tool": {
            const run = runs[item.runId];
            const view = run?.toolCalls[item.callId];
            if (!view) return null;
            return (
              <div className="event tool-event" key={item.id}>
                <ToolCallCard view={view} />
              </div>
            );
          }
          case "approval": {
            const run = runs[item.runId];
            const pending = run?.pendingApproval;
            if (!pending || pending.call.id !== item.callId) return null;
            return (
              <div className="event" key={item.id}>
                <ApprovalCard
                  pending={pending}
                  busy={busyCallId === item.callId}
                  onDecision={(approved) => onDecision(item.callId, approved)}
                />
              </div>
            );
          }
          case "status":
            return (
              <div className={`event status-event status-${item.tone}`} key={item.id}>
                <span>{item.text}</span>
                <time>{formatTime(item.ts)}</time>
              </div>
            );
          default:
            return null;
        }
      })}
      <div ref={bottomRef} />
    </div>
  );
}
