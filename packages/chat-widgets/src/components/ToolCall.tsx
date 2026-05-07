import type { ToolCallProps } from "../slots";

export function ToolCall({ message, onApprove, onDeny }: ToolCallProps) {
  const body = formatBody(message.result ?? message.args);
  const showActions = message.state === "pending" && (onApprove || onDeny);
  return (
    <div className="lutin-chat__tool">
      <div className="lutin-chat__tool-head">
        <span className="lutin-chat__tool-name">{message.name}</span>
        <span className="lutin-chat__tool-state" data-state={message.state}>
          {message.state}
        </span>
      </div>
      {body && <div className="lutin-chat__tool-body">{body}</div>}
      {message.state === "failed" && message.error && (
        <div className="lutin-chat__tool-body" style={{ color: "var(--chat-err)" }}>
          {message.error}
        </div>
      )}
      {showActions && (
        <div className="lutin-chat__tool-actions">
          {onApprove && (
            <button
              type="button"
              className="lutin-chat__approve"
              onClick={() => onApprove(message.id)}
            >
              Approve
            </button>
          )}
          {onDeny && (
            <button
              type="button"
              className="lutin-chat__deny"
              onClick={() => onDeny(message.id)}
            >
              Deny
            </button>
          )}
        </div>
      )}
    </div>
  );
}

function formatBody(value: unknown): string | null {
  if (value == null) return null;
  if (typeof value === "string") return value;
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}
