import type { Diff, ToolResult } from "./types";

export function diffsOf(result?: ToolResult): Diff[] {
  if (!result) return [];
  if (result.diff) return [result.diff];
  const fromMetadata = result.metadata?.diffs;
  if (Array.isArray(fromMetadata)) return fromMetadata as Diff[];
  return [];
}

export function formatDuration(ms?: number): string {
  if (ms === undefined) return "";
  if (ms < 1000) return `${ms} ms`;
  return `${(ms / 1000).toFixed(2)} s`;
}

export function formatTokens(value: number): string {
  if (value < 1000) return String(value);
  return `${(value / 1000).toFixed(1)}k`;
}

export function basename(path: string): string {
  const parts = path.replaceAll("\\", "/").split("/");
  return parts[parts.length - 1] || path;
}

export function prettyJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

export function statusTone(status: string): string {
  switch (status) {
    case "succeeded":
    case "completed":
      return "ok";
    case "failed":
    case "timed_out":
    case "cancelled":
    case "denied":
      return "bad";
    case "awaiting_approval":
    case "paused":
      return "warn";
    default:
      return "busy";
  }
}
