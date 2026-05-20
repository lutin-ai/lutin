import { useState } from "react";
import { CodeBlock, langFromPath } from "./CodeBlock";

export interface WriteToolViewProps {
  path: string;
  content: string;
  /** Lines shown before the fade in collapsed state. Default 8. */
  previewLines?: number;
  /** First line number for the gutter. Used by Read when the content
   *  came from a `--offset` slice and shouldn't be renumbered from 1. */
  startLine?: number;
}

export function WriteToolView({ path, content, previewLines = 8, startLine = 1 }: WriteToolViewProps) {
  const [expanded, setExpanded] = useState(false);
  const lines = content.split("\n");
  const total = lines.length;
  const truncated = total > previewLines;
  const shown = expanded || !truncated ? content : lines.slice(0, previewLines).join("\n");
  const language = langFromPath(path);
  return (
    <div className="lutin-write">
      <div className="lutin-write__body" data-truncated={truncated && !expanded}>
        <CodeBlock code={shown} language={language} startLine={startLine} />
        {truncated && !expanded && <div className="lutin-write__fade" aria-hidden="true" />}
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
          {expanded ? "Collapse" : `Show all ${total} lines`}
        </button>
      )}
    </div>
  );
}
