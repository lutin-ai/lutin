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
  // Auto-grow is handled in CSS via `field-sizing: content` (see
  // `.lutin-chat__composer-input` in `theme.css`). The previous JS
  // implementation forced a synchronous reflow on every keystroke
  // (write `height = "auto"`, read `scrollHeight`), which scaled with
  // the size of the scrollback above and froze the UI on long
  // transcripts. CSS field-sizing does the same job natively with no
  // reflow loop.

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
        className="lutin-chat__composer-input"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={onKeyDown}
        placeholder={placeholder}
        disabled={disabled}
        rows={1}
        // WebKitGTK pipes every keystroke through enchant for spell-
        // checking; on a system that has enchant but no aspell/hunspell
        // backend, enchant stalls per character and freezes typing for
        // seconds. Chat input doesn't need spellcheck, so disable it
        // alongside the related ergonomics that don't apply here.
        spellCheck={false}
        autoCorrect="off"
        autoCapitalize="off"
        autoComplete="off"
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
