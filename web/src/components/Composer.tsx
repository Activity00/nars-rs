import { useState } from "react";

interface Props {
  disabled: boolean;
  busy: boolean;
  onSend: (text: string) => Promise<void>;
}

const SUGGESTIONS = [
  "Inspect this repository and summarize the architecture.",
  "Find the failing test and fix it.",
  "Add a short README section for the new agent runtime.",
];

export function Composer({ disabled, busy, onSend }: Props) {
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);

  const submit = async () => {
    const value = text.trim();
    if (!value || disabled || sending) return;
    setSending(true);
    try {
      await onSend(value);
      setText("");
    } finally {
      setSending(false);
    }
  };

  return (
    <div className="composer">
      {text.length === 0 && !disabled && (
        <div className="suggestions">
          {SUGGESTIONS.map((suggestion) => (
            <button key={suggestion} className="chip" onClick={() => setText(suggestion)}>
              {suggestion}
            </button>
          ))}
        </div>
      )}
      <div className="composer-row">
        <textarea
          value={text}
          disabled={disabled}
          placeholder={
            disabled
              ? "Select or create a session to start."
              : "Describe a task. Enter to send, Shift+Enter for a new line."
          }
          onChange={(event) => setText(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey) {
              event.preventDefault();
              void submit();
            }
          }}
          rows={3}
        />
        <button
          className="btn primary send"
          disabled={disabled || busy || sending || !text.trim()}
          onClick={() => void submit()}
        >
          Send
        </button>
      </div>
    </div>
  );
}
