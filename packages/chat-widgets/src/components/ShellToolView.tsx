import { useState } from "react";

export interface ShellToolViewProps {
  output: string;
  /** Tail lines shown before the fade in collapsed state. Default 8. */
  tailLines?: number;
}

/** Terminal-output preview: shows the last N lines by default so users
 *  see the most actionable bit (errors, results, prompts), with a
 *  fade-at-top + expander to reveal the full transcript. */
export function ShellToolView({ output, tailLines = 8 }: ShellToolViewProps) {
  const [expanded, setExpanded] = useState(false);
  const lines = output.length === 0 ? [] : output.split("\n");
  const total = lines.length;
  const truncated = total > tailLines;
  const shown = expanded || !truncated ? output : lines.slice(-tailLines).join("\n");
  return (
    <div className="lutin-shell">
      <div className="lutin-shell__body" data-truncated={truncated && !expanded}>
        {truncated && !expanded && <div className="lutin-shell__fade" aria-hidden="true" />}
        <pre className="lutin-shell__out">{shown.length === 0 ? "(no output)" : shown}</pre>
      </div>
      {truncated && (
        <button
          type="button"
          className="lutin-write__toggle"
          onClick={(e) => {
            e.stopPropagation();
            setExpanded((v) => !v);
          }}
        >
          {expanded ? "Show last lines only" : `Show all ${total} lines`}
        </button>
      )}
    </div>
  );
}
