import { useMemo, useState, type ReactNode } from "react";
import type { SessionMeta, ToolManifest } from "../types";
import { basename } from "../lib";
import { FolderIcon, WorkspacePicker, type RecentWorkspace } from "./WorkspacePicker";

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
  const [pickerOpen, setPickerOpen] = useState(false);
  const [tokenDraft, setTokenDraft] = useState(token);

  const recent = useMemo<RecentWorkspace[]>(() => {
    const latest = new Map<string, { path: string; label: string; at: number }>();
    for (const session of sessions) {
      const current = latest.get(session.workspace);
      if (!current || session.updated_at_ms > current.at) {
        latest.set(session.workspace, {
          path: session.workspace,
          label: session.title || "Untitled",
          at: session.updated_at_ms,
        });
      }
    }
    return [...latest.values()]
      .sort((left, right) => right.at - left.at)
      .slice(0, 5)
      .map(({ path, label }) => ({ path, label }));
  }, [sessions]);

  const ids = new Set(sessions.map((session) => session.session_id));
  const children = new Map<string, SessionMeta[]>();
  const roots: SessionMeta[] = [];
  for (const session of sessions) {
    const parent = session.parent_session_id;
    if (parent && ids.has(parent)) {
      const list = children.get(parent) ?? [];
      list.push(session);
      children.set(parent, list);
    } else {
      roots.push(session);
    }
  }

  const renderSession = (
    session: SessionMeta,
    depth: number,
    visited: Set<string>,
  ): ReactNode => {
    const nextVisited = new Set(visited);
    nextVisited.add(session.session_id);
    const descendants = (children.get(session.session_id) ?? []).filter(
      (child) => !nextVisited.has(child.session_id),
    );
    return (
      <li key={session.session_id}>
        <button
          className={`session-item ${selectedId === session.session_id ? "selected" : ""}`}
          style={{ paddingLeft: `${10 + depth * 14}px` }}
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
          <div
            className="session-actions"
            style={{ paddingLeft: `${10 + depth * 14}px` }}
          >
            <button className="link-button" onClick={() => onRename(session)}>
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
        {descendants.length > 0 && (
          <ul className="session-children">
            {descendants.map((child) =>
              renderSession(child, depth + 1, nextVisited),
            )}
          </ul>
        )}
      </li>
    );
  };

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
        <span className="field-label">Workspace</span>
        <button
          type="button"
          className="workspace-trigger"
          aria-haspopup="dialog"
          aria-expanded={pickerOpen}
          onClick={() => setPickerOpen(true)}
        >
          <span className="workspace-trigger-icon">
            <FolderIcon size={13} />
          </span>
          <span className="workspace-trigger-text">
            {workspace ? basename(workspace) : "Choose a folder…"}
          </span>
          <span className="workspace-trigger-chevron" aria-hidden="true">
            ⌄
          </span>
        </button>
        {workspace && (
          <div className="workspace-chosen mono" title={workspace}>
            {workspace}
          </div>
        )}
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

      {pickerOpen && (
        <WorkspacePicker
          initialPath={workspace || undefined}
          recent={recent}
          onPick={(path) => {
            setWorkspace(path);
            setPickerOpen(false);
          }}
          onClose={() => setPickerOpen(false)}
        />
      )}

      <section className="sessions">
        <h2>Sessions</h2>
        {sessions.length === 0 && <div className="muted small">No sessions yet.</div>}
        <ul>
          {roots.map((session) => renderSession(session, 0, new Set()))}
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
