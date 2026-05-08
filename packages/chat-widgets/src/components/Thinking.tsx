import { useState } from "react";
import { Markdown } from "../markdown";
import type { ThinkingProps } from "../slots";
import { MetricsHeader } from "./MessageBubble";
import { useMessageMenu } from "./MessageActions";

export function Thinking({ message, actions }: ThinkingProps) {
  const [open, setOpen] = useState(message.streaming ?? false);
  const streaming = !!message.streaming;
  const menu = useMessageMenu({
    id: message.id,
    text: message.text,
    actions: streaming ? undefined : actions,
  });
  return (
    <details
      className="lutin-chat__thinking"
      open={open}
      onToggle={(e) => setOpen((e.target as HTMLDetailsElement).open)}
      onContextMenu={menu.onContextMenu}
    >
      <summary className="lutin-chat__thinking-head">
        <span className="lutin-chat__thinking-label">thinking</span>
        <MetricsHeader meta={message.meta} />
        <span className="lutin-chat__thinking-toggle" aria-hidden>
          {open ? "−" : "+"}
        </span>
      </summary>
      <div className="lutin-chat__thinking-body">
        {menu.editing ? menu.editor : <Markdown text={message.text} />}
      </div>
      {menu.menu}
    </details>
  );
}
