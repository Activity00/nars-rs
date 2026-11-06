import type { ReactNode } from "react";

interface Props {
  text: string;
}

/** Minimal Markdown renderer: fenced code blocks, inline code, bold, paragraphs. */
export function RichText({ text }: Props) {
  const parts = text.split(/```/);
  return (
    <div className="rich-text">
      {parts.map((part, index) =>
        index % 2 === 1 ? (
          <pre className="code-block" key={index}>
            <code>{stripLanguage(part)}</code>
          </pre>
        ) : (
          <div className="prose" key={index}>
            {renderInline(part)}
          </div>
        ),
      )}
    </div>
  );
}

function stripLanguage(part: string): string {
  const newline = part.indexOf("\n");
  if (newline === -1) return part;
  const first = part.slice(0, newline).trim();
  if (first && !first.includes(" ")) return part.slice(newline + 1);
  return part;
}

function renderInline(text: string): ReactNode[] {
  const nodes: ReactNode[] = [];
  const pattern = /(`[^`]+`|\*\*[^*]+\*\*)/g;
  let lastIndex = 0;
  let match: RegExpExecArray | null;
  let key = 0;
  while ((match = pattern.exec(text)) !== null) {
    if (match.index > lastIndex) {
      nodes.push(text.slice(lastIndex, match.index));
    }
    const token = match[0];
    if (token.startsWith("`")) {
      nodes.push(<code key={key++}>{token.slice(1, -1)}</code>);
    } else {
      nodes.push(<strong key={key++}>{token.slice(2, -2)}</strong>);
    }
    lastIndex = match.index + token.length;
  }
  if (lastIndex < text.length) nodes.push(text.slice(lastIndex));
  return nodes;
}

export function formatTime(ts: number): string {
  const date = new Date(ts || Date.now());
  return date.toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

export function shortId(value: string, length = 10): string {
  return value.length <= length ? value : `${value.slice(0, length)}…`;
}

export function statusLabel(status: string): string {
  return status.replaceAll("_", " ");
}
