import type { MemorySummary } from "../types";

interface Props {
  memory?: MemorySummary;
  loading?: boolean;
  onRefresh: () => void;
  onClose: () => void;
}

function percent(value: number): string {
  return `${(value * 100).toFixed(0)}%`;
}

export function MemoryPanel({ memory, loading, onRefresh, onClose }: Props) {
  if (!memory && !loading) return null;
  return (
    <div className="compare-overlay" onClick={onClose}>
      <div
        className="compare-panel"
        onClick={(event) => event.stopPropagation()}
        role="dialog"
        aria-label="Long-term memory"
      >
        <div className="compare-header">
          <div>
            <div className="eyebrow">Long-term memory</div>
            <h2>{memory?.workspace ?? "loading…"}</h2>
            {memory?.available ? (
              <div className="muted small">
                {memory.concept_count} concepts · {memory.implication_count} implications ·{" "}
                {memory.atom_count} atoms
              </div>
            ) : (
              <div className="muted small">
                No memory stored for this workspace yet. Run a task first.
              </div>
            )}
          </div>
          <div className="topbar-actions">
            <button className="btn ghost small" onClick={onRefresh} disabled={loading}>
              {loading ? "Refreshing…" : "Refresh"}
            </button>
            <button className="btn ghost small" onClick={onClose}>
              Close
            </button>
          </div>
        </div>

        {memory && memory.top_concepts.length > 0 && (
          <section>
            <h3>Concepts</h3>
            <table className="compare-table">
              <thead>
                <tr>
                  <th>Term</th>
                  <th>Priority</th>
                  <th>Uses</th>
                  <th>Belief</th>
                  <th>Implications</th>
                </tr>
              </thead>
              <tbody>
                {memory.top_concepts.map((concept) => (
                  <tr key={concept.term}>
                    <td>
                      <code>{concept.term}</code>
                    </td>
                    <td>{concept.priority.toFixed(2)}</td>
                    <td>{concept.use_count}</td>
                    <td>
                      {percent(concept.frequency)} / {percent(concept.confidence)}
                    </td>
                    <td>{concept.implications}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            <div className="muted small">
              Belief is frequency / confidence. Priority drives which concepts NARS looks at
              first.
            </div>
          </section>
        )}

        {memory && memory.top_implications.length > 0 && (
          <section>
            <h3>Learned implications</h3>
            <table className="compare-table">
              <thead>
                <tr>
                  <th>From</th>
                  <th>To</th>
                  <th>Truth</th>
                </tr>
              </thead>
              <tbody>
                {memory.top_implications.map((implication) => (
                  <tr key={`${implication.from}->${implication.to}`}>
                    <td>
                      <code>{implication.from}</code>
                    </td>
                    <td>
                      <code>{implication.to}</code>
                    </td>
                    <td>
                      {percent(implication.frequency)} / {percent(implication.confidence)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </section>
        )}

        {memory && memory.available && memory.concept_count === 0 && (
          <div className="muted small">
            Memory exists but is empty — no beliefs were committed yet.
          </div>
        )}
      </div>
    </div>
  );
}
