import { getCurrentWindow } from "@tauri-apps/api/window";
import { useApp } from "../store";
import styles from "./TopBar.module.css";

export function TopBar() {
  const view = useApp((s) => s.view);
  const setView = useApp((s) => s.setView);
  const conn = useApp((s) => s.conn);

  const win = getCurrentWindow();

  return (
    <header className={styles.bar} data-tauri-drag-region>
      <div className={styles.brand} data-tauri-drag-region>
        <div className={styles.logo} aria-hidden />
        <span className={styles.wordmark}>lutin</span>
      </div>

      <nav className={styles.nav}>
        <button
          className={styles.tab}
          data-active={view.kind === "settings"}
          onClick={() =>
            setView(view.kind === "settings" ? { kind: "project" } : { kind: "settings" })
          }
        >
          settings
        </button>
      </nav>

      <div className={styles.spacer} data-tauri-drag-region />

      <div className={styles.right}>
        <span className={styles.kbd} title="Command palette (not yet wired)">⌘ K</span>
        <span className={styles.conn} data-state={conn.kind}>
          <span className={styles.dot} />
          {connLabel(conn.kind)}
          {conn.kind === "rejected" && <span className={styles.connDetail}>{conn.reason}</span>}
          {conn.kind === "error" && <span className={styles.connDetail}>{conn.error}</span>}
        </span>

        <div className={styles.winCtrls}>
          <button
            className={styles.winBtn}
            title="Minimize"
            onClick={() => win.minimize()}
            aria-label="Minimize"
          >
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
              <path d="M2 5h6" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" />
            </svg>
          </button>
          <button
            className={styles.winBtn}
            title="Maximize"
            onClick={() => win.toggleMaximize()}
            aria-label="Maximize"
          >
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
              <rect x="2" y="2" width="6" height="6" stroke="currentColor" strokeWidth="1.2" />
            </svg>
          </button>
          <button
            className={`${styles.winBtn} ${styles.winClose}`}
            title="Close"
            onClick={() => win.close()}
            aria-label="Close"
          >
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
              <path d="M2.5 2.5l5 5M7.5 2.5l-5 5" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" />
            </svg>
          </button>
        </div>
      </div>
    </header>
  );
}

function connLabel(kind: string): string {
  switch (kind) {
    case "connecting": return "connecting";
    case "connected": return "connected";
    case "disconnected": return "disconnected";
    case "rejected": return "rejected";
    case "error": return "error";
    case "noconfig": return "no connection";
    default: return kind;
  }
}
