interface Props {
  diff: string;
  path?: string;
}

/** Renders a unified diff with line-level coloring. */
export function DiffView({ diff, path }: Props) {
  if (!diff.trim()) return null;
  return (
    <div className="diff">
      {path && <div className="diff-path">{path}</div>}
      <pre>
        {diff.split("\n").map((line, index) => {
          let className = "diff-line";
          if (line.startsWith("+++") || line.startsWith("---")) className += " diff-meta";
          else if (line.startsWith("@@")) className += " diff-hunk";
          else if (line.startsWith("+")) className += " diff-add";
          else if (line.startsWith("-")) className += " diff-del";
          return (
            <div className={className} key={index}>
              {line || " "}
            </div>
          );
        })}
      </pre>
    </div>
  );
}
