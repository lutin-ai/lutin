import { useEffect, useRef } from "react";
import type { ComposerProps } from "../slots";

export function Composer({
  value,
  onChange,
  onSubmit,
  onCancel,
  busy,
  placeholder = "Send a message…",
  disabled = false,
}: ComposerProps) {
  const ref = useRef<HTMLTextAreaElement>(null);

  // Auto-resize the textarea to fit content up to its CSS max-height.
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [value]);

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey && !e.altKey && !e.ctrlKey && !e.metaKey) {
      e.preventDefault();
      if (!busy && value.trim().length > 0) onSubmit();
    }
  };

  return (
    <div className="lutin-chat__composer">
      <div className="lutin-chat__composer-row">
      <textarea
        ref={ref}
        className="lutin-chat__composer-input"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={onKeyDown}
        placeholder={placeholder}
        disabled={disabled}
        rows={1}
      />
      {busy && onCancel ? (
        <button type="button" className="lutin-chat__cancel" onClick={onCancel}>
          Cancel
        </button>
      ) : (
        <button
          type="button"
          className="lutin-chat__send"
          onClick={onSubmit}
          disabled={disabled || value.trim().length === 0}
        >
          Send
        </button>
      )}
      </div>
    </div>
  );
}
