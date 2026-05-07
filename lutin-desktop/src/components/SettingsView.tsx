import { useEffect, useMemo, useRef, useState } from "react";
import {
  cpSendOk,
  keybindBackend,
  settingsGet,
  settingsSet,
  type KeybindBackendInfo,
} from "../api";
import { useApp } from "../store";
import type {
  Action,
  ConnectionProfile,
  DesktopSettings,
  KeyBind,
  ProviderConfig,
  ProviderKind,
  Target,
  WorkflowId,
  WorkflowInfo,
} from "../types";
import styles from "./SettingsView.module.css";

const PROVIDER_KINDS: { value: ProviderKind; label: string }[] = [
  { value: "open_router", label: "OpenRouter" },
  { value: "anthropic", label: "Anthropic" },
  { value: "ollama", label: "Ollama" },
  { value: "open_ai_compat", label: "OpenAI-compatible" },
];

type Tab = "connections" | "keybinds" | "providers";

const ACTION_LABELS: Record<Action["kind"], string> = {
  ptt: "Push-to-talk (PTT)",
};

const TARGET_LABELS: Record<Target["kind"], string> = {
  active_workflow: "Active workflow",
  workflow: "Pinned workflow",
  clipboard: "Clipboard",
};

export function SettingsView() {
  const settings = useApp((s) => s.settings);
  const setSettings = useApp((s) => s.setSettings);
  const setView = useApp((s) => s.setView);
  const conn = useApp((s) => s.conn);

  const [tab, setTab] = useState<Tab>("connections");

  useEffect(() => {
    if (!settings) settingsGet().then((s) => setSettings(s));
  }, [settings, setSettings]);

  return (
    <main className={styles.pane}>
      <header className={styles.header}>
        <div className={styles.crumbs}>
          <span>app</span>
          <span className={styles.crumbSep}>/</span>
          <span>settings</span>
        </div>
        <div className={styles.headerRow}>
          <h1 className={styles.title}>Settings</h1>
          <button className={styles.ghostBtn} onClick={() => setView({ kind: "project" })}>
            Close
          </button>
        </div>
        <nav className={styles.tabs}>
          <button
            className={styles.tab}
            data-active={tab === "connections"}
            onClick={() => setTab("connections")}
          >
            Connections
          </button>
          <button
            className={styles.tab}
            data-active={tab === "keybinds"}
            onClick={() => setTab("keybinds")}
          >
            Keybinds
          </button>
          <button
            className={styles.tab}
            data-active={tab === "providers"}
            onClick={() => setTab("providers")}
          >
            LLM providers
          </button>
        </nav>
      </header>

      <div className={styles.body}>
        {tab === "connections" && settings && <ConnectionsPanel initial={settings} />}
        {tab === "connections" && !settings && <Loading />}
        {tab === "keybinds" && settings && (
          <KeybindsPanel initial={settings} connected={conn.kind === "connected"} />
        )}
        {tab === "keybinds" && !settings && <Loading />}
        {tab === "providers" && <ProvidersPanel connected={conn.kind === "connected"} />}
      </div>
    </main>
  );
}

function Loading() {
  return <div className={styles.loading}>Loading settings…</div>;
}

/* ───────── Connections ───────── */

function ConnectionsPanel({ initial }: { initial: DesktopSettings }) {
  const setSettings = useApp((s) => s.setSettings);
  const [draft, setDraft] = useState<DesktopSettings>(initial);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const dirty = JSON.stringify(draft) !== JSON.stringify(initial);

  const updateProfile = (idx: number, patch: Partial<ConnectionProfile>) => {
    setDraft({
      ...draft,
      connections: draft.connections.map((p, i) => (i === idx ? { ...p, ...patch } : p)),
    });
  };
  const removeProfile = (idx: number) =>
    setDraft({ ...draft, connections: draft.connections.filter((_, i) => i !== idx) });
  const addProfile = () =>
    setDraft({
      ...draft,
      connections: [
        ...draft.connections,
        { name: `cp-${draft.connections.length + 1}`, addr: "127.0.0.1:7000", token: "" },
      ],
    });

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      await settingsSet(draft);
      setSettings(draft);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      <Card
        title="Default connection"
        description="Used on startup when multiple control-panel profiles are configured."
      >
        <Field label="Profile">
          <select
            className={styles.input}
            value={draft.default}
            onChange={(e) => setDraft({ ...draft, default: e.target.value })}
          >
            <option value="">(first available)</option>
            {draft.connections.map((c) => (
              <option key={c.name} value={c.name}>{c.name}</option>
            ))}
          </select>
        </Field>
      </Card>

      <Card
        title="Control panel connections"
        description="Each profile points at a CP instance. Address is host:port; the token must match the CP's configured shared secret."
        action={
          <button className={styles.addBtn} onClick={addProfile}>+ Add</button>
        }
      >
        {draft.connections.length === 0 && (
          <Empty>No connections configured.</Empty>
        )}
        {draft.connections.map((c, i) => (
          <div key={i} className={styles.row}>
            <Field label="Name">
              <input
                className={styles.input}
                value={c.name}
                onChange={(e) => updateProfile(i, { name: e.target.value })}
              />
            </Field>
            <Field label="Address">
              <input
                className={styles.input}
                placeholder="127.0.0.1:7000"
                value={c.addr}
                onChange={(e) => updateProfile(i, { addr: e.target.value })}
              />
            </Field>
            <Field label="Token">
              <input
                className={styles.input}
                type="password"
                value={c.token}
                onChange={(e) => updateProfile(i, { token: e.target.value })}
              />
            </Field>
            <button
              className={styles.iconBtn}
              title="Remove connection"
              onClick={() => removeProfile(i)}
            >
              ×
            </button>
          </div>
        ))}
      </Card>

      {error && <div className={styles.error}>{error}</div>}

      <SaveBar
        dirty={dirty}
        saving={saving}
        onSave={save}
        onRevert={() => setDraft(initial)}
        label="Save & reconnect"
      />
    </>
  );
}

/* ───────── Providers ───────── */

function ProvidersPanel({ connected }: { connected: boolean }) {
  const [providers, setProviders] = useState<ProviderConfig[] | null>(null);
  const [initial, setInitial] = useState<ProviderConfig[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (!connected) return;
    cpSendOk("ListProviders")
      .then((r) => {
        if (typeof r === "object" && "Providers" in r) {
          setProviders(r.Providers);
          setInitial(r.Providers);
        }
      })
      .catch((e) => setError(String(e)));
  }, [connected]);

  if (!connected) {
    return (
      <Card
        title="LLM providers"
        description="Provider configuration lives on the control panel."
      >
        <Empty>Connect to a control panel to view and edit providers.</Empty>
      </Card>
    );
  }
  if (providers == null) {
    return (
      <Card title="LLM providers">
        <Empty>Loading…</Empty>
      </Card>
    );
  }

  const update = (idx: number, patch: Partial<ProviderConfig>) =>
    setProviders(providers.map((p, i) => (i === idx ? { ...p, ...patch } : p)));
  const remove = (idx: number) => setProviders(providers.filter((_, i) => i !== idx));
  const add = (kind: ProviderKind) => {
    const base: ProviderConfig = {
      name: defaultName(kind, providers),
      kind,
      use_oauth: false,
    };
    if (kind === "ollama") base.base_url = "http://localhost:11434";
    setProviders([...providers, base]);
  };

  const dirty = JSON.stringify(providers) !== JSON.stringify(initial);

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      const cleaned = providers.map(stripBlanks);
      await cpSendOk({ SetProviders: { providers: cleaned } });
      setInitial(providers);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      <Card
        title="LLM providers"
        description="Credentials stay on the control panel. Use the env-var field to reference a secret loaded into the CP's environment instead of pasting a key here."
      >
        {providers.length === 0 && <Empty>No providers configured. Add one below.</Empty>}
        <div className={styles.providerList}>
          {providers.map((p, i) => (
            <ProviderCard
              key={i}
              provider={p}
              onChange={(patch) => update(i, patch)}
              onRemove={() => remove(i)}
            />
          ))}
        </div>
        <div className={styles.addRow}>
          <span className={styles.addLabel}>Add provider:</span>
          {PROVIDER_KINDS.map((k) => (
            <button key={k.value} className={styles.addBtn} onClick={() => add(k.value)}>
              + {k.label}
            </button>
          ))}
        </div>
      </Card>

      {error && <div className={styles.error}>{error}</div>}

      <SaveBar
        dirty={dirty}
        saving={saving}
        onSave={save}
        onRevert={() => initial && setProviders(initial)}
        label="Save providers"
      />
    </>
  );
}

interface ProviderCardProps {
  provider: ProviderConfig;
  onChange: (patch: Partial<ProviderConfig>) => void;
  onRemove: () => void;
}

function ProviderCard({ provider, onChange, onRemove }: ProviderCardProps) {
  const showOauth = provider.kind === "anthropic";
  const showBaseUrl =
    provider.kind === "ollama" || provider.kind === "open_ai_compat";

  return (
    <div className={styles.providerCard}>
      <div className={styles.providerHead}>
        <span className={styles.providerKind}>
          {PROVIDER_KINDS.find((k) => k.value === provider.kind)?.label ?? provider.kind}
        </span>
        <input
          className={`${styles.input} ${styles.providerName}`}
          value={provider.name}
          onChange={(e) => onChange({ name: e.target.value })}
        />
        <button className={styles.iconBtn} title="Remove" onClick={onRemove}>×</button>
      </div>

      <div className={styles.providerGrid}>
        <Field label="Kind">
          <select
            className={styles.input}
            value={provider.kind}
            onChange={(e) => onChange({ kind: e.target.value as ProviderKind })}
          >
            {PROVIDER_KINDS.map((k) => (
              <option key={k.value} value={k.value}>{k.label}</option>
            ))}
          </select>
        </Field>
        <Field label="API key">
          <input
            className={styles.input}
            type="password"
            placeholder={provider.api_key_env ? `from $${provider.api_key_env}` : "sk-…"}
            value={provider.api_key ?? ""}
            onChange={(e) => onChange({ api_key: e.target.value || null })}
          />
        </Field>
        <Field label="API key env var" hint="Loaded by the CP at startup.">
          <input
            className={styles.input}
            placeholder="OPENROUTER_API_KEY"
            value={provider.api_key_env ?? ""}
            onChange={(e) => onChange({ api_key_env: e.target.value || null })}
          />
        </Field>
        {showBaseUrl && (
          <Field label="Base URL">
            <input
              className={styles.input}
              placeholder="https://api.example.com"
              value={provider.base_url ?? ""}
              onChange={(e) => onChange({ base_url: e.target.value || null })}
            />
          </Field>
        )}
        {showOauth && (
          <label className={styles.checkbox}>
            <input
              type="checkbox"
              checked={!!provider.use_oauth}
              onChange={(e) => onChange({ use_oauth: e.target.checked })}
            />
            <span>Use OAuth (Anthropic)</span>
          </label>
        )}
      </div>
    </div>
  );
}

/* ───────── Keybinds ───────── */

function KeybindsPanel({
  initial,
  connected,
}: {
  initial: DesktopSettings;
  connected: boolean;
}) {
  const setSettings = useApp((s) => s.setSettings);
  // Drafts only the keybind list — everything else in `initial`
  // (connections, default, whisper) is round-tripped untouched at save
  // time. Diffing the array directly avoids the JSON.stringify-the-
  // whole-struct trap where an unrelated field reference change
  // would falsely report dirty.
  const [draft, setDraft] = useState<KeyBind[]>(initial.keybinds);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [workflows, setWorkflows] = useState<WorkflowInfo[]>([]);
  const [workflowsError, setWorkflowsError] = useState<string | null>(null);
  const [backend, setBackend] = useState<KeybindBackendInfo | null>(null);
  const [backendError, setBackendError] = useState<string | null>(null);
  const dirty = JSON.stringify(draft) !== JSON.stringify(initial.keybinds);

  useEffect(() => {
    // Don't paper over an IPC failure with a default backend — the
    // settings UI's "configure in your compositor" branch hinges on
    // the backend value, so showing the X11/macOS/Windows view to a
    // Wayland user who hit a transient invoke failure is worse than
    // saying "couldn't determine".
    keybindBackend()
      .then((b) => {
        setBackend(b);
        setBackendError(null);
      })
      .catch((e) => setBackendError(String(e)));
  }, []);

  useEffect(() => {
    if (!connected) {
      setWorkflows([]);
      setWorkflowsError(null);
      return;
    }
    cpSendOk("ListWorkflows")
      .then((r) => {
        if (typeof r === "object" && "Workflows" in r) {
          setWorkflows(r.Workflows);
          setWorkflowsError(null);
        }
      })
      .catch((e) => setWorkflowsError(String(e)));
  }, [connected]);

  const removeBind = (idx: number) =>
    setDraft(draft.filter((_, i) => i !== idx));
  const addBind = (bind: KeyBind) => setDraft([...draft, bind]);

  const conflictsInDraft = useMemo(() => {
    const combos = draft.map((b) => b.combo);
    return combos.filter((c, i) => combos.indexOf(c) !== i);
  }, [draft]);

  const save = async () => {
    if (conflictsInDraft.length > 0) {
      setError(`Duplicate combos: ${[...new Set(conflictsInDraft)].join(", ")}`);
      return;
    }
    setSaving(true);
    setError(null);
    try {
      const next = { ...initial, keybinds: draft };
      await settingsSet(next);
      setSettings(next);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      <Card
        title="Global hotkeys"
        description={
          "Combos work even when the app is unfocused. PTT holds the key " +
          "to record audio; release transcribes it into the chosen target. " +
          "If no target is bound (or the active workflow can't receive " +
          "transcription) text falls through to the clipboard so audio " +
          "isn't lost."
        }
      >
        {backendError && (
          <div className={styles.error}>
            Couldn't determine keybind backend: {backendError}
          </div>
        )}
        {backend?.kind === "portal" && <PortalNotice />}
        {draft.length === 0 && (
          <Empty>
            No hotkeys configured. The default <code>Numpad1 → PTT →
            Active workflow</code> is registered until you add your first
            binding.
          </Empty>
        )}
        {draft.map((b, i) => (
          <KeybindRow
            key={i}
            index={i}
            bind={b}
            workflows={workflows}
            backend={backend}
            conflict={conflictsInDraft.includes(b.combo)}
            onRemove={() => removeBind(i)}
          />
        ))}
        <KeybindAdder
          existingCombos={draft.map((b) => b.combo)}
          workflows={workflows}
          workflowsError={workflowsError}
          onAdd={addBind}
        />
      </Card>

      {error && <div className={styles.error}>{error}</div>}

      <SaveBar
        dirty={dirty}
        saving={saving}
        onSave={save}
        onRevert={() => setDraft(initial.keybinds)}
        label="Save keybinds"
      />
    </>
  );
}

function KeybindRow({
  index,
  bind,
  workflows,
  backend,
  conflict,
  onRemove,
}: {
  index: number;
  bind: KeyBind;
  workflows: WorkflowInfo[];
  backend: KeybindBackendInfo | null;
  conflict: boolean;
  onRemove: () => void;
}) {
  // Build the snippet from the backend-supplied template. The id
  // prefix and template literal are owned by Rust, so any wire
  // change there propagates here without an edit.
  const snippet =
    backend?.kind === "portal"
      ? backend.snippet_template
          .replace("{combo}", bind.combo || "<KEY>")
          .replace("{id}", `${backend.id_prefix}${index}`)
      : null;
  return (
    <div className={styles.row} data-conflict={conflict || undefined}>
      <Field label="Combo">
        <code className={styles.comboPill}>{bind.combo}</code>
      </Field>
      <Field label="Action">
        <span>{ACTION_LABELS[bind.action.kind]}</span>
      </Field>
      <Field label="Target">
        <span>{describeTarget(bind.target, workflows)}</span>
      </Field>
      <button className={styles.iconBtn} title="Remove keybind" onClick={onRemove}>
        ×
      </button>
      {snippet && (
        <div className={styles.portalSnippet}>
          <span className={styles.portalSnippetLabel}>Hyprland</span>
          <code
            className={styles.portalSnippetCode}
            onClick={(e) => copyToClipboard(e.currentTarget.textContent ?? "")}
            title="Click to copy"
          >
            {snippet}
          </code>
        </div>
      )}
    </div>
  );
}

function copyToClipboard(text: string) {
  if (navigator.clipboard) {
    void navigator.clipboard.writeText(text);
  }
}

function PortalNotice() {
  return (
    <div className={styles.portalNotice}>
      <strong>Wayland session:</strong> the compositor owns global
      shortcuts. The combo column below is a hint shown in the portal
      dialog — to actually bind a key, copy the per-row snippet into
      your <code>hyprland.conf</code> (or your compositor's
      equivalent). Re-saving from this UI restarts the portal session;
      ids stay stable as long as you don't reorder rows.
    </div>
  );
}

function describeTarget(target: Target, workflows: WorkflowInfo[]): string {
  switch (target.kind) {
    case "active_workflow":
      return TARGET_LABELS.active_workflow;
    case "clipboard":
      return TARGET_LABELS.clipboard;
    case "workflow": {
      const wf = workflows.find((w) => w.id === target.workflow);
      return wf ? `${wf.icon} ${wf.display_name}` : `Workflow: ${target.workflow}`;
    }
  }
}

/// In-progress target shape. Mirrors `Target` except the workflow id
/// can be empty while the user picks one — making the partial state a
/// proper variant rather than a sibling field eliminates the "stale id
/// when target switches away from workflow" bug. `submit` only fires
/// once `workflow` is non-empty, so the conversion to `Target` is
/// total at that point.
type TargetDraft =
  | { kind: "active_workflow" }
  | { kind: "clipboard" }
  | { kind: "workflow"; workflow: WorkflowId | "" };

const TARGET_DRAFTS: Record<Target["kind"], TargetDraft> = {
  active_workflow: { kind: "active_workflow" },
  clipboard: { kind: "clipboard" },
  workflow: { kind: "workflow", workflow: "" },
};

function KeybindAdder({
  existingCombos,
  workflows,
  workflowsError,
  onAdd,
}: {
  existingCombos: string[];
  workflows: WorkflowInfo[];
  workflowsError: string | null;
  onAdd: (bind: KeyBind) => void;
}) {
  const [combo, setCombo] = useState<string>("");
  const [actionKind, setActionKind] = useState<Action["kind"]>("ptt");
  const [target, setTarget] = useState<TargetDraft>({ kind: "active_workflow" });

  const conflict = combo !== "" && existingCombos.includes(combo);
  // For `Target::Workflow` we require not just a non-empty id but one
  // that's actually in the catalogue — guards against stale ids if
  // workflows change between load and save, and stops empty-catalogue
  // submission when `ListWorkflows` failed.
  const targetReady =
    target.kind !== "workflow" ||
    (target.workflow !== "" &&
      workflows.some((w) => w.id === target.workflow));
  const canAdd = combo !== "" && !conflict && targetReady;

  const submit = () => {
    if (!canAdd) return;
    if (target.kind === "workflow" && target.workflow === "") return;
    onAdd({
      combo,
      action: { kind: actionKind },
      target: target as Target,
    });
    setCombo("");
  };

  return (
    <div className={styles.kbAdder}>
      <Field label="Combo" hint="Click the field, then press your hotkey. Esc cancels.">
        <ComboCapture value={combo} onChange={setCombo} />
        {conflict && (
          <span className={styles.fieldHint} style={{ color: "var(--err)" }}>
            Already bound — pick another combo or remove the existing entry.
          </span>
        )}
      </Field>
      <Field label="Action">
        <select
          className={styles.input}
          value={actionKind}
          onChange={(e) => setActionKind(e.target.value as Action["kind"])}
        >
          {Object.entries(ACTION_LABELS).map(([k, label]) => (
            <option key={k} value={k}>{label}</option>
          ))}
        </select>
      </Field>
      <Field label="Target">
        <select
          className={styles.input}
          value={target.kind}
          onChange={(e) =>
            setTarget(TARGET_DRAFTS[e.target.value as Target["kind"]])
          }
        >
          {Object.entries(TARGET_LABELS).map(([k, label]) => (
            <option key={k} value={k}>{label}</option>
          ))}
        </select>
        {target.kind === "workflow" && (
          <>
            <select
              className={styles.input}
              style={{ marginTop: 6 }}
              value={target.workflow}
              onChange={(e) =>
                setTarget({ kind: "workflow", workflow: e.target.value })
              }
            >
              <option value="">
                {workflows.length === 0
                  ? "No workflows available"
                  : "Select workflow…"}
              </option>
              {workflows.map((w) => (
                <option key={w.id} value={w.id}>
                  {w.icon} {w.display_name}
                </option>
              ))}
            </select>
            {workflowsError && (
              <span
                className={styles.fieldHint}
                style={{ color: "var(--err)" }}
              >
                Couldn't load workflows: {workflowsError}
              </span>
            )}
          </>
        )}
      </Field>
      <button className={styles.primary} disabled={!canAdd} onClick={submit}>
        Add
      </button>
    </div>
  );
}

/// Click-to-capture combo input. Mirrors the `tauri-plugin-global-shortcut`
/// accelerator format: `Modifier+Modifier+Key`. Modifiers are emitted as
/// `Control` / `Shift` / `Alt` / `Super`; the key half is taken from
/// `KeyboardEvent.code` and normalised (e.g. `KeyM` → `M`, `Digit1` → `1`,
/// `Numpad1` stays). Keys that are *only* modifiers don't register as a
/// complete combo — we wait for a real key. Esc clears.
function ComboCapture({
  value,
  onChange,
}: {
  value: string;
  onChange: (combo: string) => void;
}) {
  const [capturing, setCapturing] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  const onKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.stopPropagation();
    if (e.key === "Escape") {
      onChange("");
      ref.current?.blur();
      return;
    }
    const key = normaliseKey(e.code, e.key);
    if (!key) return; // pure-modifier press; wait for the real key.
    const mods: string[] = [];
    if (e.ctrlKey) mods.push("Control");
    if (e.altKey) mods.push("Alt");
    if (e.shiftKey) mods.push("Shift");
    if (e.metaKey) mods.push("Super");
    onChange([...mods, key].join("+"));
    ref.current?.blur();
  };

  return (
    <div
      ref={ref}
      className={styles.comboCapture}
      data-capturing={capturing || undefined}
      tabIndex={0}
      onFocus={() => setCapturing(true)}
      onBlur={() => setCapturing(false)}
      onKeyDown={onKeyDown}
      role="textbox"
      aria-label="Combo capture"
    >
      {capturing
        ? "Press a key combo…"
        : value || "Click to capture"}
    </div>
  );
}

function normaliseKey(code: string, key: string): string | null {
  // Pure modifiers — ignore until a real key arrives.
  if (
    code === "ControlLeft" || code === "ControlRight" ||
    code === "ShiftLeft" || code === "ShiftRight" ||
    code === "AltLeft" || code === "AltRight" ||
    code === "MetaLeft" || code === "MetaRight"
  ) return null;

  if (code.startsWith("Key") && code.length === 4) return code.slice(3);
  if (code.startsWith("Digit") && code.length === 6) return code.slice(5);
  if (code.startsWith("Numpad")) return code; // Numpad0..Numpad9, NumpadAdd, …
  if (code.startsWith("F") && /^F\d{1,2}$/.test(code)) return code;
  if (code === "ArrowUp") return "Up";
  if (code === "ArrowDown") return "Down";
  if (code === "ArrowLeft") return "Left";
  if (code === "ArrowRight") return "Right";
  if (code === "Space") return "Space";
  if (code === "Enter") return "Enter";
  if (code === "Tab") return "Tab";
  if (code === "Backspace") return "Backspace";
  if (code === "Minus") return "Minus";
  if (code === "Equal") return "Equal";
  if (code === "Comma") return "Comma";
  if (code === "Period") return "Period";
  if (code === "Slash") return "Slash";
  if (code === "Backslash") return "Backslash";
  if (code === "Semicolon") return "Semicolon";
  if (code === "Quote") return "Quote";
  if (code === "BracketLeft") return "BracketLeft";
  if (code === "BracketRight") return "BracketRight";
  // Last-ditch: the printable key as-is. Lets unusual layouts still
  // produce *something* the user can recognise; the Rust strict parse
  // will reject it on save if the accelerator crate doesn't know it.
  return key.length === 1 ? key.toUpperCase() : code;
}

/* ───────── primitives ───────── */

function Card({
  title,
  description,
  action,
  children,
}: {
  title: string;
  description?: string;
  action?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <section className={styles.card}>
      <div className={styles.cardHead}>
        <div>
          <h2 className={styles.cardTitle}>{title}</h2>
          {description && <p className={styles.cardDesc}>{description}</p>}
        </div>
        {action}
      </div>
      <div className={styles.cardBody}>{children}</div>
    </section>
  );
}

function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <label className={styles.field}>
      <span className={styles.fieldLabel}>{label}</span>
      {children}
      {hint && <span className={styles.fieldHint}>{hint}</span>}
    </label>
  );
}

function Empty({ children }: { children: React.ReactNode }) {
  return <div className={styles.empty}>{children}</div>;
}

function SaveBar({
  dirty,
  saving,
  onSave,
  onRevert,
  label,
}: {
  dirty: boolean;
  saving: boolean;
  onSave: () => void;
  onRevert: () => void;
  label: string;
}) {
  return (
    <div className={styles.saveBar} data-visible={dirty || saving}>
      <span className={styles.saveStatus}>
        {dirty ? "Unsaved changes" : "All changes saved"}
      </span>
      <div className={styles.saveActions}>
        <button
          className={styles.ghostBtn}
          disabled={!dirty || saving}
          onClick={onRevert}
        >
          Revert
        </button>
        <button
          className={styles.primary}
          disabled={!dirty || saving}
          onClick={onSave}
        >
          {saving ? "Saving…" : label}
        </button>
      </div>
    </div>
  );
}

function defaultName(kind: ProviderKind, existing: ProviderConfig[]): string {
  const base = kind.replace(/_/g, "-");
  let n = base;
  let i = 2;
  while (existing.some((p) => p.name === n)) n = `${base}-${i++}`;
  return n;
}

function stripBlanks(p: ProviderConfig): ProviderConfig {
  return {
    name: p.name,
    kind: p.kind,
    api_key: p.api_key && p.api_key.length > 0 ? p.api_key : null,
    api_key_env: p.api_key_env && p.api_key_env.length > 0 ? p.api_key_env : null,
    base_url: p.base_url && p.base_url.length > 0 ? p.base_url : null,
    use_oauth: p.use_oauth ?? false,
  };
}
