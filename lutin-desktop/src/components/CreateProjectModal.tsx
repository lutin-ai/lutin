import { useState } from "react";
import { cpSendOk } from "../api";
import { Modal } from "./Modal";
import styles from "./Modal.module.css";

export interface CreateProjectModalProps {
  onClose: () => void;
}

export function CreateProjectModal({ onClose }: CreateProjectModalProps) {
  const [slug, setSlug] = useState("");
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async () => {
    if (!slug.trim() || !name.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await cpSendOk({
        CreateProject: { slug: slug.trim(), display_name: name.trim() },
      });
      onClose();
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };

  return (
    <Modal onClose={onClose}>
      <div className={styles.header}>
        <span className={styles.title}>New project</span>
      </div>
      <div style={{ display: "flex", flexDirection: "column", gap: 8, padding: 16 }}>
        <input
          className={styles.input}
          style={{ border: "1px solid var(--border-2)", borderRadius: "var(--r-md)", padding: "8px 10px", fontSize: "var(--fs-xl)" }}
          placeholder="slug (e.g. my-experiment)"
          value={slug}
          onChange={(e) => setSlug(e.target.value)}
          autoFocus
        />
        <input
          className={styles.input}
          style={{ border: "1px solid var(--border-2)", borderRadius: "var(--r-md)", padding: "8px 10px", fontSize: "var(--fs-xl)" }}
          placeholder="display name"
          value={name}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && submit()}
        />
        {error && <div style={{ color: "var(--err)", fontSize: "var(--fs-md)" }}>{error}</div>}
        <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
          <button onClick={onClose} disabled={busy}>Cancel</button>
          <button
            onClick={submit}
            disabled={busy || !slug.trim() || !name.trim()}
            style={{ background: "var(--accent)", color: "var(--accent-on)", border: "none", padding: "6px 14px", borderRadius: "var(--r-md)" }}
          >
            Create
          </button>
        </div>
      </div>
    </Modal>
  );
}
