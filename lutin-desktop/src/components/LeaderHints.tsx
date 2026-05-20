import { APP_ACTION_LABELS, useAppKeybinds, type AppAction } from "../appKeybinds";
import styles from "./LeaderHints.module.css";

/// Small floating overlay that appears while a leader chord is in
/// flight. Shows every binding whose combo starts with the active
/// leader so the user can discover continuations without memorising
/// the table. Disappears on selection, escape, or timeout.
export function LeaderHints() {
  const leader = useAppKeybinds((s) => s.pendingLeader);
  const binds = useAppKeybinds((s) => s.binds);

  if (!leader) return null;

  const matches = binds
    .map((b) => {
      const parts = b.combo.trim().toLowerCase().split(/\s+/);
      if (parts[0] !== leader || parts.length !== 2) return null;
      return { action: b.action as AppAction, key: parts[1] };
    })
    .filter((x): x is { action: AppAction; key: string } => x !== null);

  return (
    <div className={styles.overlay}>
      <div className={styles.panel}>
        <div className={styles.head}>
          <span className={styles.leaderKey}>{leader}</span>
          <span className={styles.leaderHint}>press next key…</span>
        </div>
        {matches.length === 0 ? (
          <div className={styles.empty}>No chords bound under <code>{leader}</code>.</div>
        ) : (
          <ul className={styles.list}>
            {matches.map(({ action, key }) => (
              <li key={action} className={styles.row}>
                <span className={styles.key}>{key}</span>
                <span className={styles.label}>{APP_ACTION_LABELS[action] ?? action}</span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
