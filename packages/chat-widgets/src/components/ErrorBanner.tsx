import type { ErrorBannerProps } from "../slots";

export function ErrorBanner({ message, onDismiss }: ErrorBannerProps) {
  return (
    <div className="lutin-chat__error" role="alert">
      <span>{message}</span>
      {onDismiss && (
        <button
          type="button"
          className="lutin-chat__error-dismiss"
          onClick={onDismiss}
          aria-label="Dismiss"
        >
          ×
        </button>
      )}
    </div>
  );
}
