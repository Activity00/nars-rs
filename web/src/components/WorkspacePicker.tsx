import {
  Fragment,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";
import { api } from "../api";
import type { DirEntry, DirListing } from "../types";
import { basename } from "../lib";

export interface RecentWorkspace {
  path: string;
  label: string;
}

interface Props {
  initialPath?: string;
  recent: RecentWorkspace[];
  onPick: (path: string) => void;
  onClose: () => void;
}

type Row = { kind: "dir"; entry: DirEntry };

export function FolderIcon({ size = 14 }: { size?: number }) {
  return (
    <svg
      viewBox="0 0 16 16"
      width={size}
      height={size}
      fill="none"
      stroke="currentColor"
      strokeWidth="1.3"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M1.6 4.3c0-.6.5-1.1 1.1-1.1h2.9l1.4 1.7h6.3c.6 0 1.1.5 1.1 1.1v5.7c0 .6-.5 1.1-1.1 1.1H2.7c-.6 0-1.1-.5-1.1-1.1V4.3Z" />
    </svg>
  );
}

function looksLikePath(value: string): boolean {
  return value.startsWith("/") || value.startsWith("~") || /^[A-Za-z]:[\\/]/.test(value);
}

/** Breadcrumb segments for an absolute path. */
function trail(path: string): { label: string; path: string }[] {
  const parts = path.replaceAll("\\", "/").split("/").filter(Boolean);
  let accumulated = "";
  return parts.map((part) => {
    accumulated = `${accumulated}/${part}`;
    return { label: part, path: accumulated };
  });
}

function message(error: unknown): string {
  return String(error).replace(/^Error:\s*/, "");
}

/**
 * A folder browser for choosing a session workspace.
 *
 * The daemon exposes a directory-only listing, so the picker can be navigated
 * with the mouse or the keyboard; typing an absolute path still works for
 * anyone who would rather paste one.
 */
export function WorkspacePicker({ initialPath, recent, onPick, onClose }: Props) {
  const [listing, setListing] = useState<DirListing>();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();
  const [filter, setFilter] = useState("");
  const [showHidden, setShowHidden] = useState(false);
  const [active, setActive] = useState(-1);
  const listRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  const load = useCallback(async (path?: string) => {
    setLoading(true);
    setError(undefined);
    try {
      const next = await api.browse(path);
      setListing(next);
      setFilter("");
      setActive(-1);
    } catch (cause) {
      setError(message(cause));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load(initialPath);
  }, [load, initialPath]);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const rows = useMemo<Row[]>(() => {
    const query = filter.trim().toLowerCase();
    const searching = query.length > 0 && !looksLikePath(filter.trim());
    return (listing?.entries ?? [])
      .filter((entry) => showHidden || !entry.hidden)
      .filter((entry) => !searching || entry.name.toLowerCase().includes(query))
      .map((entry) => ({ kind: "dir", entry }));
  }, [listing, filter, showHidden]);

  const scrollRowIntoView = useCallback((index: number) => {
    listRef.current
      ?.querySelector<HTMLElement>(`[data-row="${index}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, []);

  const step = useCallback(
    (delta: number) => {
      if (rows.length === 0) return;
      const next = Math.min(Math.max(active + delta, 0), rows.length - 1);
      setActive(next);
      scrollRowIntoView(next);
    },
    [active, rows.length, scrollRowIntoView],
  );

  const choose = useCallback(() => {
    if (listing && !loading) onPick(listing.path);
  }, [listing, loading, onPick]);

  const onKeyDown = useCallback(
    (event: KeyboardEvent) => {
      switch (event.key) {
        case "Escape":
          event.stopPropagation();
          onClose();
          return;
        case "ArrowDown":
          event.preventDefault();
          step(1);
          return;
        case "ArrowUp":
          event.preventDefault();
          step(-1);
          return;
        case "ArrowLeft":
          if (!filter) {
            event.preventDefault();
            if (listing?.parent) void load(listing.parent);
          }
          return;
        case "Enter": {
          const typed = filter.trim();
          if (event.metaKey || event.ctrlKey) {
            event.preventDefault();
            choose();
            return;
          }
          if (looksLikePath(typed)) {
            event.preventDefault();
            void load(typed);
            return;
          }
          const row = rows[active];
          if (row) {
            event.preventDefault();
            void load(row.entry.path);
            return;
          }
          event.preventDefault();
          choose();
          return;
        }
        default:
      }
    },
    [active, choose, filter, listing, load, onClose, rows, step],
  );

  const crumbs = trail(listing?.path ?? "");
  const typedPath = filter.trim();

  return (
    <div
      className="picker-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        className="picker"
        role="dialog"
        aria-modal="true"
        aria-label="Choose a workspace folder"
        onKeyDown={onKeyDown}
      >
        <header className="picker-head">
          <div>
            <h2>Choose a workspace</h2>
            <p className="muted small">
              Sessions work inside one folder. Browse to it, or paste an absolute path.
            </p>
          </div>
          <button className="picker-close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>

        {recent.length > 0 && (
          <section className="picker-section">
            <span className="picker-legend">Recent</span>
            <div className="picker-chips">
              {recent.map((item) => (
                <button
                  key={item.path}
                  className="picker-chip"
                  title={item.path}
                  onClick={() => onPick(item.path)}
                >
                  <FolderIcon size={12} />
                  <span className="picker-chip-name">{basename(item.path)}</span>
                  <span className="picker-chip-path mono">{item.path}</span>
                </button>
              ))}
            </div>
          </section>
        )}

        <section className="picker-section picker-browser">
          <div className="picker-toolbar">
            <button
              className="picker-nav"
              title="Parent folder (←)"
              disabled={!listing?.parent}
              onClick={() => listing?.parent && void load(listing.parent)}
            >
              ↑
            </button>
            <button
              className="picker-nav"
              title="Home folder"
              disabled={!listing?.home || listing.home === listing.path}
              onClick={() => listing?.home && void load(listing.home)}
            >
              Home
            </button>
            <input
              ref={inputRef}
              className="picker-filter"
              placeholder="Filter folders, or type /absolute/path"
              value={filter}
              onChange={(event) => {
                setFilter(event.target.value);
                setActive(-1);
              }}
            />
            <label className="picker-toggle" title="Show hidden folders">
              <input
                type="checkbox"
                checked={showHidden}
                onChange={(event) => setShowHidden(event.target.checked)}
              />
              hidden
            </label>
          </div>

          <div className="picker-crumbs">
            {crumbs.map((crumb, index) => (
              <Fragment key={crumb.path}>
                {index > 0 && <span className="picker-sep">/</span>}
                <button
                  className={`picker-crumb${index === crumbs.length - 1 ? " current" : ""}`}
                  disabled={index === crumbs.length - 1}
                  onClick={() => void load(crumb.path)}
                >
                  {crumb.label}
                </button>
              </Fragment>
            ))}
          </div>

          <div className="picker-list" ref={listRef} role="listbox" aria-label="Folders">
            {loading && <div className="picker-empty muted">Loading…</div>}
            {!loading && error && (
              <div className="picker-error">
                <div>{error}</div>
                {looksLikePath(typedPath) && (
                  <button className="btn ghost small" onClick={() => onPick(typedPath)}>
                    Use {typedPath} anyway
                  </button>
                )}
              </div>
            )}
            {!loading && !error && rows.length === 0 && (
              <div className="picker-empty muted">No folders here.</div>
            )}
            {!loading &&
              !error &&
              rows.map((row, index) => (
                <button
                  key={row.entry.path}
                  data-row={index}
                  role="option"
                  aria-selected={index === active}
                  className={`picker-row${index === active ? " active" : ""}`}
                  onMouseEnter={() => setActive(index)}
                  onClick={() => void load(row.entry.path)}
                >
                  <span className="picker-row-icon">
                    <FolderIcon size={13} />
                  </span>
                  <span className="picker-row-name">{row.entry.name}</span>
                  {row.entry.hidden && <span className="tag">hidden</span>}
                </button>
              ))}
            {truncatedNote(listing)}
          </div>
        </section>

        <footer className="picker-foot">
          <div className="picker-target mono" title={listing?.path}>
            {listing?.path ?? "—"}
          </div>
          <div className="picker-actions">
            <button className="btn ghost" onClick={onClose}>
              Cancel
            </button>
            <button className="btn primary" disabled={!listing || loading} onClick={choose}>
              Use this folder
            </button>
          </div>
        </footer>
      </div>
    </div>
  );
}

function truncatedNote(listing?: DirListing) {
  if (!listing?.truncated) return null;
  return (
    <div className="picker-empty muted small">
      Showing the first {listing.entries.length} folders — type a path to jump straight there.
    </div>
  );
}
