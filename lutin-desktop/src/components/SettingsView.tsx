import { useEffect, useState } from "react";
import { settingsGet, settingsSet } from "../api";
import { useApp } from "../store";
import type { ConnectionProfile, DesktopSettings } from "../types";
import styles from "./SettingsView.module.css";

export function SettingsView() {
  const settings = useApp((s) => s.settings);
  const setSettings = useApp((s) => s.setSettings);
  const setView = useApp((s) => s.setView);

  const [draft, setDraft] = useState<DesktopSettings | null>(settings);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (!settings) {
      settingsGet().then((s) => {
        setSettings(s);
        setDraft(s);
      });
    }
  }, [settings, setSettings]);

  if (!draft) return <div className={styles.loading}>Loading settings…</div>;

  const updateProfile = (idx: number, patch: Partial<ConnectionProfile>) => {
    setDraft({
      ...draft,
      connections: draft.connections.map((p, i) =>
        i === idx ? { ...p, ...patch } : p,
      ),
    });
  };

  const removeProfile = (idx: number) => {
    setDraft({
      ...draft,
      connections: draft.connections.filter((_, i) => i !== idx),
    });
  };

  const addProfile = () => {
    setDraft({
      ...draft,
      connections: [
        ...draft.connections,
        { name: `cp-${draft.connections.length + 1}`, addr: "127.0.0.1:7000", token: "" },
      ],
    });
  };

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      await settingsSet(draft);
      setSettings(draft);
      setView({ kind: "project" });
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <main className={styles.pane}>
      <header className={styles.header}>
        <h1>Settings</h1>
        <button onClick={() => setView({ kind: "project" })}>Back</button>
      </header>

      <section className={styles.section}>
        <label className={styles.label}>
          Default connection
          <select
            value={draft.default}
            onChange={(e) => setDraft({ ...draft, default: e.target.value })}
          >
            <option value="">(first)</option>
            {draft.connections.map((c) => (
              <option key={c.name} value={c.name}>{c.name}</option>
            ))}
          </select>
        </label>
      </section>

      <section className={styles.section}>
        <h2>Control panel connections</h2>
        {draft.connections.map((c, i) => (
          <div key={i} className={styles.profile}>
            <label>
              Name
              <input
                value={c.name}
                onChange={(e) => updateProfile(i, { name: e.target.value })}
              />
            </label>
            <label>
              Address (<code>host:port</code>)
              <input
                value={c.addr}
                onChange={(e) => updateProfile(i, { addr: e.target.value })}
              />
            </label>
            <label>
              Token
              <input
                type="password"
                value={c.token}
                onChange={(e) => updateProfile(i, { token: e.target.value })}
              />
            </label>
            <button className={styles.removeBtn} onClick={() => removeProfile(i)}>
              Remove
            </button>
          </div>
        ))}
        <button className={styles.addBtn} onClick={addProfile}>+ Add connection</button>
      </section>

      {error && <div className={styles.error}>{error}</div>}

      <footer className={styles.footer}>
        <button className={styles.primary} disabled={saving} onClick={save}>
          {saving ? "Saving…" : "Save & reconnect"}
        </button>
      </footer>
    </main>
  );
}
