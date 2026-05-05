import { useState } from "react";
import type { Lutin } from "./lutin";

interface Props { lutin: Lutin }

/// Phase 2 chat UI stub. The bytes pump (chrome ↔ engine) isn't wired
/// yet, so this only proves bundle delivery + the lutin handshake.
/// `lutin.request` / `lutin.onBroadcast` start working once the engine
/// bridge lands; until then this file just renders context and lets
/// the user fire a chrome notification.
export function App({ lutin }: Props) {
  const [status, setStatus] = useState<string>("");

  const ping = () => {
    lutin.notify(
      `${lutin.workflow}/${lutin.session.slice(0, 8)} says hi`,
      "Chat plugin",
    );
    setStatus("notification sent");
    setTimeout(() => setStatus(""), 1500);
  };

  return (
    <div style={styles.root}>
      <h1 style={styles.title}>{lutin.manifest.icon} Chat</h1>
      <dl style={styles.dl}>
        <dt style={styles.dt}>Project</dt>
        <dd style={styles.dd}><code>{lutin.slug}</code></dd>
        <dt style={styles.dt}>Session</dt>
        <dd style={styles.dd}><code>{lutin.session}</code></dd>
        <dt style={styles.dt}>Permissions</dt>
        <dd style={styles.dd}>
          {lutin.manifest.permissions.length === 0
            ? <em>none declared</em>
            : lutin.manifest.permissions.join(", ")}
        </dd>
      </dl>
      <p style={styles.note}>
        Engine bridge is wired (lutin.request / lutin.onBroadcast
        round-trip through chrome). Chat protocol decoding lands next.
      </p>
      <button style={styles.button} onClick={ping}>
        Send test notification
      </button>
      {status && <span style={styles.status}>{status}</span>}
    </div>
  );
}

const styles: Record<string, React.CSSProperties> = {
  root: {
    fontFamily:
      "system-ui, -apple-system, 'Segoe UI', sans-serif",
    padding: "1.5rem",
    color: "#222",
    background: "#fafafa",
    minHeight: "100vh",
    boxSizing: "border-box",
  },
  title: { fontSize: "1.4rem", margin: "0 0 1rem" },
  dl: { display: "grid", gridTemplateColumns: "auto 1fr", gap: "0.25rem 1rem", margin: 0 },
  dt: { color: "#666", fontSize: "0.85rem" },
  dd: { margin: 0, fontSize: "0.9rem" },
  note: { marginTop: "1.5rem", color: "#888", fontStyle: "italic" },
  button: {
    marginTop: "0.5rem",
    padding: "0.4rem 0.9rem",
    border: "1px solid #ccc",
    borderRadius: 4,
    background: "white",
    cursor: "pointer",
  },
  status: { marginLeft: "0.75rem", color: "#0a7", fontSize: "0.85rem" },
};
