import { useState } from "react";
import { cpSendOk } from "../api";
import { useApp } from "../store";
import styles from "./Sidebar.module.css";

export function Sidebar() {
  const projects = useApp((s) => s.projects);
  const selected = useApp((s) => s.selectedProject);
  const select = useApp((s) => s.selectProject);
  const view = useApp((s) => s.view);
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

  const onSelect = (slug: string) => {
    if (view.kind === "settings") setView({ kind: "project" });
    select(slug);
  };

  const inSettings = view.kind === "settings";

  return (
    <aside className={styles.sidebar}>
      <div className={styles.sectionHead}>
        <span className={styles.sectionLabel}>Projects</span>
        <button
          className={styles.addBtn}
          title="New project"
          disabled={conn.kind !== "connected" || creating}
          onClick={() => setCreating(true)}
        >
          +
        </button>
      </div>

      <ul className={styles.list}>
        {projects.map((p) => (
          <li
            key={p.slug}
            className={!inSettings && selected === p.slug ? styles.active : undefined}
            onClick={() => onSelect(p.slug)}
          >
            <span className={styles.folderIcon} aria-hidden>
              <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.4">
                <path d="M1.5 4.5a1 1 0 0 1 1-1h3.2a1 1 0 0 1 .7.3l1 1h6.1a1 1 0 0 1 1 1v6.7a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1z" />
              </svg>
            </span>
            <span className={styles.projName}>{p.display_name}</span>
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

      {creating && (
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
            <button className={styles.primary} onClick={submit}>Create</button>
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
      )}
    </aside>
  );
}
