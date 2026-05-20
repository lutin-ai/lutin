import type { HeaderProps } from "../slots";

// Streaming state is rendered by the composer (busy spinner + Cancel),
// so the header only surfaces error/maxRounds — and even those are
// also covered by the ErrorBanner below it. Keep it lean: render
// nothing for streaming, fall back to a minimal error chip otherwise.
export function Header({ turn }: HeaderProps) {
  if (turn.kind === "idle" || turn.kind === "streaming") return null;
  if (turn.kind === "errored") {
    return (
      <div className="lutin-chat__header">
        <span>error: {turn.message}</span>
      </div>
    );
  }
  return null;
}
