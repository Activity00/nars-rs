import { useState } from "react";
import type { SessionMeta, ToolManifest } from "../types";
import { basename } from "../lib";

interface Props {
  sessions: SessionMeta[];
  selectedId?: string;
  tools: ToolManifest[];
  connection: "connecting" | "live" | "error";
  provider?: string;
  model?: string;
  token: string;
  onSelect: (id: string) => void;
  onCreate: (workspace: string) => void;
  onRename: (session: SessionMeta) => void;
  onDelete: (session: SessionMeta) => void;
  onSaveToken: (token: string) => void;
}

export function Sidebar({
  sessions,
  selectedId,
  tools,
  connection,
  provider,
  model,
  token,
  onSelect,
  onCreate,
  onRename,
  onDelete,
  onSaveToken,
}: Props) {
  const [workspace, setWorkspace] = useState("");
  const [tokenDraft, setTokenDraft] = useState(token);

  return (
    <aside className="sidebar">
      <div className="brand">
        <div className="brand-mark">N</div>
        <div>
          <div className="brand-name">nars-agent</div>
          <div className="muted small">{provider ?? "offline"} · {model ?? "no model"}</div>
        </div>
      </div>

      <div className={`connection connection-${connection}`}>
        <span className="dot" />
        {connection === "live"
          ? "event stream live"
          : connection === "connecting"
            ? "connecting…"
            : "stream disconnected"}
      </div>

      <section className="new-session">
        <label htmlFor="workspace">Workspace</label>
        <input
          id="workspace"
          placeholder="/absolute/path/to/project"
          value={workspace}
          onChange={(event) => setWorkspace(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && workspace.trim()) {
              onCreate(workspace.trim());
              setWorkspace("");
            }
          }}
        />
        <button
          className="btn primary"
          disabled={!workspace.trim()}
          onClick={() => {
            onCreate(workspace.trim());
            setWorkspace("");
          }}
        >
          New session
        </button>
      </section>

      <section className="sessions">
        <h2>Sessions</h2>
        {sessions.length === 0 && <div className="muted small">No sessions yet.</div>}
        <ul>
          {sessions.map((session) => (
            <li key={session.session_id}>
              <button
                className={`session-item ${selectedId === session.session_id ? "selected" : ""}`}
                onClick={() => onSelect(session.session_id)}
              >
                <span className="session-title">
                  {session.parent_session_id && (
                    <span className="session-branch" title="Branched session">
                      ↳{" "}
                    </span>
                  )}
                  {session.title || "Untitled"}
                </span>
                <span className="session-workspace mono small">
                  {basename(session.workspace)}
                </span>
              </button>
              {selectedId === session.session_id && (
                <div className="session-actions">
                  <button
                    className="link-button"
                    onClick={() => onRename(session)}
                  >
                    Rename
                  </button>
                  <button
                    className="link-button danger"
                    onClick={() => onDelete(session)}
                  >
                    Delete
                  </button>
                </div>
              )}
            </li>
          ))}
        </ul>
      </section>

      <details className="tools">
        <summary>Tools ({tools.length})</summary>
        <ul>
          {tools.map((tool) => (
            <li key={tool.name}>
              <span className="mono">{tool.name}</span>
              <span className={`tag tag-${tool.side_effect}`}>{tool.side_effect}</span>
            </li>
          ))}
        </ul>
      </details>

      <details className="token-box">
        <summary>Auth token</summary>
        <div className="token-row">
          <input
            type="password"
            placeholder="optional bearer token"
            value={tokenDraft}
            onChange={(event) => setTokenDraft(event.target.value)}
          />
          <button className="btn ghost" onClick={() => onSaveToken(tokenDraft.trim())}>
            Save
          </button>
        </div>
      </details>
    </aside>
  );
}
