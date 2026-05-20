import { useEffect, useMemo, useRef, useState } from "react";
import {
  audioInputDevices,
  audioOutputDevices,
  cpSendOk,
  keybindBackend,
  settingsGet,
  settingsSet,
  type KeybindBackendInfo,
} from "../api";
import { APP_ACTION_LABELS, useAppKeybinds, type AppAction } from "../appKeybinds";
import { useQuickChat } from "../quickChat";
import { useApp } from "../store";
import type {
  Action,
  ConnectionProfile,
  DesktopSettings,
  KeyBind,
  ParakeetConfig,
  ParakeetModel,
  ProviderConfig,
  ProviderKind,
  SttConfig,
  Target,
  WebSearchSettings,
  WhisperConfig,
  WhisperModel,
  WorkflowId,
  WorkflowInfo,
} from "../types";
import styles from "./SettingsView.module.css";
import { Select } from "./Select";

const PROVIDER_KINDS: { value: ProviderKind; label: string }[] = [
  { value: "open_router", label: "OpenRouter" },
  { value: "anthropic", label: "Anthropic" },
  { value: "ollama", label: "Ollama" },
  { value: "open_ai_compat", label: "OpenAI-compatible" },
];

type Tab = "connections" | "keybinds" | "audio" | "stt" | "providers" | "web_search";

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
            data-active={tab === "audio"}
            onClick={() => setTab("audio")}
          >
            Audio
          </button>
          <button
            className={styles.tab}
            data-active={tab === "stt"}
            onClick={() => setTab("stt")}
          >
            Speech-to-text
          </button>
          <button
            className={styles.tab}
            data-active={tab === "providers"}
            onClick={() => setTab("providers")}
          >
            LLM providers
          </button>
          <button
            className={styles.tab}
            data-active={tab === "web_search"}
            onClick={() => setTab("web_search")}
          >
            Web search
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
        {tab === "audio" && settings && <AudioPanel initial={settings} />}
        {tab === "audio" && !settings && <Loading />}
        {tab === "stt" && settings && <SttPanel initial={settings} />}
        {tab === "stt" && !settings && <Loading />}
        {tab === "providers" && <ProvidersPanel connected={conn.kind === "connected"} />}
        {tab === "web_search" && <WebSearchPanel connected={conn.kind === "connected"} />}
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
          <Select
            value={draft.default}
            onChange={(v) => setDraft({ ...draft, default: v })}
            options={[
              { value: "", label: "(first available)" },
              ...draft.connections.map((c) => ({ value: c.name, label: c.name })),
            ]}
          />
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

/* ───────── Audio ───────── */

function AudioPanel({ initial }: { initial: DesktopSettings }) {
  const setSettings = useApp((s) => s.setSettings);
  const [draft, setDraft] = useState(initial.audio);
  const [inputs, setInputs] = useState<string[] | null>(null);
  const [outputs, setOutputs] = useState<string[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const dirty = JSON.stringify(draft) !== JSON.stringify(initial.audio);

  useEffect(() => {
    // Re-enumerate every time the panel mounts so a hot-plugged USB
    // mic shows up without an app restart. The lists are tiny and
    // cpal enumeration is cheap.
    audioInputDevices().then(setInputs).catch((e) => setError(String(e)));
    audioOutputDevices().then(setOutputs).catch((e) => setError(String(e)));
  }, []);

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      const next = { ...initial, audio: draft };
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
        title="Microphone"
        description="Used for push-to-talk capture. Saving rebuilds the cpal stream on the new device — no restart needed."
      >
        <DevicePicker
          label="Input device"
          devices={inputs}
          value={draft.input}
          onChange={(input) => setDraft({ ...draft, input })}
        />
      </Card>

      <Card
        title="Speakers"
        description="Used for TTS playback. Switching devices clears any in-flight TTS audio because already-resampled samples target the previous device's rate."
      >
        <DevicePicker
          label="Output device"
          devices={outputs}
          value={draft.output}
          onChange={(output) => setDraft({ ...draft, output })}
        />
      </Card>

      {error && <div className={styles.error}>{error}</div>}

      <SaveBar
        dirty={dirty}
        saving={saving}
        onSave={save}
        onRevert={() => setDraft(initial.audio)}
        label="Save audio devices"
      />
    </>
  );
}

function DevicePicker({
  label,
  devices,
  value,
  onChange,
}: {
  label: string;
  devices: string[] | null;
  value: string | null;
  onChange: (value: string | null) => void;
}) {
  // Show the saved selection even when it isn't currently in the
  // enumerated list — a USB mic disappearing shouldn't silently flip
  // the dropdown to "host default" and erase the saved preference.
  const hasSaved =
    value != null && (devices?.includes(value) ?? false) === false;

  return (
    <Field label={label}>
      <Select
        value={value ?? ""}
        onChange={(v) => onChange(v === "" ? null : v)}
        disabled={devices == null}
        options={[
          { value: "", label: "Host default" },
          ...(hasSaved && value != null
            ? [{ value, label: `${value} (not currently available)` }]
            : []),
          ...(devices ?? []).map((name) => ({ value: name, label: name })),
        ]}
      />
      {devices != null && devices.length === 0 && (
        <span className={styles.fieldHint}>
          No devices reported by cpal — only "host default" is available.
        </span>
      )}
    </Field>
  );
}

/* ───────── Speech-to-text ───────── */

const WHISPER_MODELS: { value: WhisperModel; label: string }[] = [
  { value: "large-v3-turbo", label: "large-v3-turbo (recommended)" },
  { value: "distil-large-v3", label: "distil-large-v3 (smaller / faster)" },
];

const PARAKEET_MODELS: { value: ParakeetModel; label: string }[] = [
  { value: "tdt06b-v3", label: "tdt-0.6b-v3 (multilingual, 25 langs)" },
];

function defaultWhisper(): WhisperConfig {
  return { model: "large-v3-turbo", language: null, beam_size: 5 };
}

function defaultParakeet(): ParakeetConfig {
  return { model: "tdt06b-v3" };
}

function sttBackend(s: SttConfig): "Whisper" | "Parakeet" {
  return "Whisper" in s ? "Whisper" : "Parakeet";
}

function SttPanel({ initial }: { initial: DesktopSettings }) {
  const setSettings = useApp((s) => s.setSettings);
  const [draft, setDraft] = useState<SttConfig>(initial.stt);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const dirty = JSON.stringify(draft) !== JSON.stringify(initial.stt);
  const backend = sttBackend(draft);

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      const next = { ...initial, stt: draft };
      await settingsSet(next);
      setSettings(next);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const switchBackend = (next: "Whisper" | "Parakeet") => {
    if (next === backend) return;
    setDraft(next === "Whisper"
      ? { Whisper: defaultWhisper() }
      : { Parakeet: defaultParakeet() });
  };

  return (
    <>
      <Card
        title="Backend"
        description="Selected backend handles every push-to-talk transcription. Switching takes effect on the next PTT — no restart."
      >
        <label className={styles.checkbox}>
          <input
            type="radio"
            name="stt-backend"
            checked={backend === "Whisper"}
            onChange={() => switchBackend("Whisper")}
          />
          <span>Whisper (CPU/GPU, multilingual, accurate)</span>
        </label>
        <label className={styles.checkbox}>
          <input
            type="radio"
            name="stt-backend"
            checked={backend === "Parakeet"}
            onChange={() => switchBackend("Parakeet")}
          />
          <span>Parakeet (NVIDIA, ~10× faster, 25 langs)</span>
        </label>
      </Card>

      {"Whisper" in draft && (
        <WhisperFields
          config={draft.Whisper}
          onChange={(w) => setDraft({ Whisper: w })}
        />
      )}

      {"Parakeet" in draft && (
        <ParakeetFields
          config={draft.Parakeet}
          onChange={(p) => setDraft({ Parakeet: p })}
        />
      )}

      {error && <div className={styles.error}>{error}</div>}

      <SaveBar
        dirty={dirty}
        saving={saving}
        onSave={save}
        onRevert={() => setDraft(initial.stt)}
        label="Save speech-to-text"
      />
    </>
  );
}

function WhisperFields({
  config,
  onChange,
}: {
  config: WhisperConfig;
  onChange: (next: WhisperConfig) => void;
}) {
  return (
    <Card
      title="Whisper settings"
      description="whisper.cpp under the hood. First use of a model downloads its GGUF weights into the CP's model cache."
    >
      <Field label="Model">
        <Select
          value={config.model}
          onChange={(v) => onChange({ ...config, model: v as WhisperModel })}
          options={WHISPER_MODELS.map((m) => ({ value: m.value, label: m.label }))}
        />
      </Field>
      <Field
        label="Language"
        hint="Whisper language code (en, sv, …). Leave blank to auto-detect."
      >
        <input
          className={styles.input}
          placeholder="auto"
          value={config.language ?? ""}
          onChange={(e) =>
            onChange({ ...config, language: e.target.value || null })
          }
        />
      </Field>
      <Field
        label={`Beam size: ${config.beam_size <= 1 ? "1 (greedy)" : config.beam_size}`}
        hint="1 = greedy decoding (fastest); higher trades CPU for accuracy."
      >
        <input
          className={styles.input}
          type="range"
          min={1}
          max={8}
          step={1}
          value={config.beam_size <= 1 ? 1 : config.beam_size}
          onChange={(e) =>
            onChange({ ...config, beam_size: Number(e.target.value) })
          }
        />
      </Field>
    </Card>
  );
}

function ParakeetFields({
  config,
  onChange,
}: {
  config: ParakeetConfig;
  onChange: (next: ParakeetConfig) => void;
}) {
  return (
    <Card
      title="Parakeet settings"
      description="NVIDIA Parakeet TDT, ONNX runtime. First use downloads ~2.6 GB of weights into the CP's model cache."
    >
      <Field label="Model">
        <Select
          value={config.model}
          onChange={(v) => onChange({ ...config, model: v as ParakeetModel })}
          options={PARAKEET_MODELS.map((m) => ({ value: m.value, label: m.label }))}
        />
      </Field>
    </Card>
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
          <Select
            value={provider.kind}
            onChange={(v) => onChange({ kind: v as ProviderKind })}
            options={PROVIDER_KINDS.map((k) => ({ value: k.value, label: k.label }))}
          />
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

/* ───────── Web search ───────── */

function WebSearchPanel({ connected }: { connected: boolean }) {
  const [draft, setDraft] = useState<WebSearchSettings | null>(null);
  const [initial, setInitial] = useState<WebSearchSettings | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (!connected) return;
    cpSendOk("GetWebSearch")
      .then((r) => {
        if (typeof r === "object" && "WebSearch" in r) {
          setDraft(r.WebSearch);
          setInitial(r.WebSearch);
        }
      })
      .catch((e) => setError(String(e)));
  }, [connected]);

  if (!connected) {
    return (
      <Card
        title="Web search"
        description="Web-search credentials live on the control panel."
      >
        <Empty>Connect to a control panel to view and edit web-search settings.</Empty>
      </Card>
    );
  }
  if (draft == null) {
    return (
      <Card title="Web search">
        <Empty>Loading…</Empty>
      </Card>
    );
  }

  const dirty = JSON.stringify(draft) !== JSON.stringify(initial);

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      const cleaned: WebSearchSettings = {
        brave_api_key:
          draft.brave_api_key && draft.brave_api_key.length > 0
            ? draft.brave_api_key
            : null,
      };
      await cpSendOk({ SetWebSearch: { settings: cleaned } });
      setInitial(cleaned);
      setDraft(cleaned);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      <Card
        title="Brave Search"
        description="API key used by the agent's web_search tool. Free tier allows 2K queries/month. Get one at api.search.brave.com."
      >
        <Field label="API key">
          <input
            className={styles.input}
            type="password"
            placeholder="BSA…"
            value={draft.brave_api_key ?? ""}
            onChange={(e) =>
              setDraft({ ...draft, brave_api_key: e.target.value || null })
            }
          />
        </Field>
      </Card>

      {error && <div className={styles.error}>{error}</div>}

      <SaveBar
        dirty={dirty}
        saving={saving}
        onSave={save}
        onRevert={() => initial && setDraft(initial)}
        label="Save web search"
      />
    </>
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

      <AppKeybindsCard />
      <QuickChatCard />
    </>
  );
}

function QuickChatCard() {
  const projects = useApp((s) => s.projects);
  const defaultProject = useQuickChat((s) => s.defaultProject);
  const sessionPtr = useQuickChat((s) => s.sessionPtr);
  const setDefaultProject = useQuickChat((s) => s.setDefaultProject);
  const setSessionPtr = useQuickChat((s) => s.setSessionPtr);

  const ptrProject = sessionPtr ? projects.find((p) => p.slug === sessionPtr.project) : null;

  return (
    <Card
      title="Quick chat"
      description={
        "The chord bound to `openQuickChat` (default `space q`) jumps to " +
        "a persistent chat session. The default project decides where a " +
        "new quick-chat session is created when none is pinned yet; the " +
        "pinned session below is reused on subsequent presses until it " +
        "is deleted or you reset it here."
      }
    >
      <div className={styles.row}>
        <Field label="Default project">
          <Select
            value={defaultProject ?? ""}
            onChange={(v) => setDefaultProject(v || null)}
            options={[
              { value: "", label: "Use current project" },
              ...projects.map((p) => ({ value: p.slug, label: p.display_name })),
            ]}
            placeholder="Use current project"
          />
        </Field>
      </div>
      <div className={styles.row}>
        <Field label="Pinned session">
          <span>
            {sessionPtr === null
              ? "— (none yet; created on first use)"
              : `${ptrProject?.display_name ?? sessionPtr.project} · ${sessionPtr.session.slice(0, 8)}`}
          </span>
        </Field>
      </div>
      <div style={{ display: "flex", justifyContent: "flex-end", paddingTop: 8 }}>
        <button onClick={() => setSessionPtr(null)} disabled={sessionPtr === null}>
          Reset pinned session
        </button>
      </div>
    </Card>
  );
}

function AppKeybindsCard() {
  const binds = useAppKeybinds((s) => s.binds);
  const setBind = useAppKeybinds((s) => s.setBind);
  const reset = useAppKeybinds((s) => s.reset);

  return (
    <Card
      title="App shortcuts"
      description={
        "In-app combos only — they fire while this window has focus and " +
        "you're not typing in a text field. Format: a single key " +
        "(e.g. `i`), a modifier combo (`ctrl k`, `ctrl shift p`), or a " +
        "leader chord that starts with `space` (`space p`, `space f`). " +
        "Press Esc to cancel an in-flight leader."
      }
    >
      {binds.map((b) => (
        <div key={b.action} className={styles.row}>
          <Field label="Action">
            <span>{APP_ACTION_LABELS[b.action as AppAction] ?? b.action}</span>
          </Field>
          <Field label="Combo">
            <input
              className={styles.input}
              value={b.combo}
              onChange={(e) => setBind(b.action as AppAction, e.target.value)}
              spellCheck={false}
              placeholder="e.g. space p"
            />
          </Field>
        </div>
      ))}
      <div style={{ display: "flex", justifyContent: "flex-end", paddingTop: 8 }}>
        <button onClick={reset}>Reset to defaults</button>
      </div>
    </Card>
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
        <Select
          value={actionKind}
          onChange={(v) => setActionKind(v as Action["kind"])}
          options={Object.entries(ACTION_LABELS).map(([k, label]) => ({
            value: k,
            label,
          }))}
        />
      </Field>
      <Field label="Target">
        <Select
          value={target.kind}
          onChange={(v) => setTarget(TARGET_DRAFTS[v as Target["kind"]])}
          options={Object.entries(TARGET_LABELS).map(([k, label]) => ({
            value: k,
            label,
          }))}
        />
        {target.kind === "workflow" && (
          <>
            <Select
              style={{ marginTop: 6 }}
              value={target.workflow}
              onChange={(v) => setTarget({ kind: "workflow", workflow: v })}
              options={[
                {
                  value: "",
                  label:
                    workflows.length === 0
                      ? "No workflows available"
                      : "Select workflow…",
                },
                ...workflows.map((w) => ({
                  value: w.id,
                  label: `${w.icon} ${w.display_name}`,
                })),
              ]}
            />
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
