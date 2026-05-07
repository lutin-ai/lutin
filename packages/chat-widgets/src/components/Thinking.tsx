import { useState } from "react";
import { Markdown } from "../markdown";
import type { ThinkingProps } from "../slots";

export function Thinking({ message }: ThinkingProps) {
  const [open, setOpen] = useState(true);
  return (
    <details
      className="lutin-chat__thinking"
      open={open}
      onToggle={(e) => setOpen((e.target as HTMLDetailsElement).open)}
    >
      <summary className="lutin-chat__thinking-head">
        <span className="lutin-chat__thinking-label">thinking</span>
        <span className="lutin-chat__thinking-toggle" aria-hidden>
          {open ? "−" : "+"}
        </span>
      </summary>
      <div className="lutin-chat__thinking-body">
        <Markdown text={message.text} />
      </div>
    </details>
  );
}
