import type { HeaderProps } from "../slots";

export function Header({ turn, onCancel }: HeaderProps) {
  if (turn.kind === "idle") return null;
  return (
    <div className="lutin-chat__header">
      {turn.kind === "streaming" && (
        <>
          <span className="lutin-chat__streaming-dot" aria-hidden />
          <span>streaming…</span>
        </>
      )}
      {turn.kind === "errored" && <span>error: {turn.message}</span>}
      <span className="lutin-chat__header-spacer" />
      {turn.kind === "streaming" && onCancel && (
        <button
          type="button"
          className="lutin-chat__header-cancel"
          onClick={onCancel}
        >
          Cancel
        </button>
      )}
    </div>
  );
}
