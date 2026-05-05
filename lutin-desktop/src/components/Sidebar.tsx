import { useState } from "react";
import { cpSendOk } from "../api";
import { useApp } from "../store";
import styles from "./Sidebar.module.css";

export function Sidebar() {
  const projects = useApp((s) => s.projects);
  const selected = useApp((s) => s.selectedProject);
  const select = useApp((s) => s.selectProject);
  const setView = useApp((s) => s.setView);
  const conn = useApp((s) => s.conn);

  const [creating, setCreating] = useState(false);
  const [newSlug, setNewSlug] = useState("");
  const [newName, setNewName] = useState("");
  const [error, setError] = useState<string | null>(null);

  const submit = async () => {
    if (!newSlug.trim() || !newName.trim()) return;
    setError(null);
    try {
      await cpSendOk({
        CreateProject: { slug: newSlug.trim(), display_name: newName.trim() },
      });
      // Server will fan out a `ProjectCreated` event; the store's
      // `applyEvent` reducer adds it to `projects`. No manual append.
      setNewSlug("");
      setNewName("");
      setCreating(false);
    } catch (e) {
      setError(String(e));
    }
  };

  const remove = async (slug: string) => {
    setError(null);
    try {
      await cpSendOk({ DeleteProject: { slug } });
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <aside className={styles.sidebar}>
      <header className={styles.header}>
        <span className={styles.title}>lutin</span>
        <button
          className={styles.iconBtn}
          title="Settings"
          onClick={() => setView({ kind: "settings" })}
        >
          ⚙
        </button>
      </header>

      <div className={styles.connBadge} data-state={conn.kind}>
        {connLabel(conn.kind)}
        {conn.kind === "rejected" && <div className={styles.connDetail}>{conn.reason}</div>}
        {conn.kind === "error" && <div className={styles.connDetail}>{conn.error}</div>}
      </div>

      <ul className={styles.list}>
        {projects.map((p) => (
          <li
            key={p.slug}
            className={selected === p.slug ? styles.active : undefined}
            onClick={() => select(p.slug)}
          >
            <span className={styles.projName}>{p.display_name}</span>
            <span className={styles.projSlug}>{p.slug}</span>
            <button
              className={styles.deleteBtn}
              title="Delete"
              onClick={(e) => {
                e.stopPropagation();
                if (confirm(`Delete project ${p.slug}?`)) remove(p.slug);
              }}
            >
              ×
            </button>
          </li>
        ))}
      </ul>

      {creating ? (
        <div className={styles.creator}>
          <input
            placeholder="slug"
            value={newSlug}
            onChange={(e) => setNewSlug(e.target.value)}
            autoFocus
          />
          <input
            placeholder="display name"
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && submit()}
          />
          <div className={styles.creatorRow}>
            <button onClick={submit}>Create</button>
            <button
              onClick={() => {
                setCreating(false);
                setNewSlug("");
                setNewName("");
                setError(null);
              }}
            >
              Cancel
            </button>
          </div>
          {error && <div className={styles.error}>{error}</div>}
        </div>
      ) : (
        <button
          className={styles.newProjectBtn}
          disabled={conn.kind !== "connected"}
          onClick={() => setCreating(true)}
        >
          + New project
        </button>
      )}
    </aside>
  );
}

function connLabel(kind: string): string {
  switch (kind) {
    case "connecting": return "Connecting…";
    case "connected": return "Connected";
    case "disconnected": return "Disconnected";
    case "rejected": return "Rejected";
    case "error": return "Error";
    case "noconfig": return "No connection configured";
    default: return kind;
  }
}
