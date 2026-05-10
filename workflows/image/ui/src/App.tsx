import { memo, useCallback, useEffect, useState, useRef } from "react";
import "@lutin/chat-widgets/theme.css";
import {
  decodeImageEvent,
  decodeImageResponse,
  encodeImageRequest,
  imageErrorMessage,
  MODEL_DEFAULTS,
  MODEL_IDS,
  type ImageSettings,
  type TranscriptEntry,
} from "@lutin/image-protocol";
import type { Lutin } from "./lutin";
import styles from "./App.module.css";

interface TurnImage {
  imageId: string;
  mime: string;
  seed: bigint;
  ms: number;
}

interface Turn {
  id: number;
  prompt: string;
  status: "pending" | "done" | "error";
  /// Length 0 while pending; >= 1 on success (count > 1 → grid).
  /// Carries refs only — bytes live in the App-level `bytesById` map
  /// so a turn restored from disk can fill bytes in lazily without
  /// re-rendering the whole turn list.
  images: TurnImage[];
  /// `null` unless `status === "error"`.
  error: string | null;
  jobId: string | null;
  progress: { step: number; total: number } | null;
}

interface DraftOverrides {
  /// Each field is `null` to mean "use the workflow default", a value
  /// to override for this turn only. Lets us keep the composer
  /// `useState` shallow and avoid re-deriving defaults on every keystroke.
  negativePrompt: string;
  count: number | null;
  steps: number | null;
  cfg: number | null;
  width: number | null;
  height: number | null;
  seed: bigint | null;
  /// `null` means "use the settings default model". The composer's
  /// dropdown is the primary way to override per-turn; advanced
  /// fields key their placeholders off the resolved choice.
  modelId: string | null;
}

const EMPTY_OVERRIDES: DraftOverrides = {
  negativePrompt: "",
  count: null,
  steps: null,
  cfg: null,
  width: null,
  height: null,
  seed: null,
  modelId: null,
};

type Health = { state: "checking" } | { state: "ok" } | { state: "down"; message: string };

export function App({ lutin }: { lutin: Lutin }) {
  const [turns, setTurns] = useState<Turn[]>([]);
  const [busy, setBusy] = useState(false);
  const [settings, setSettings] = useState<ImageSettings | null>(null);
  const [showSettings, setShowSettings] = useState(false);
  const [health, setHealth] = useState<Health>({ state: "checking" });
  // imageId → "data:<mime>;base64,<b64>" once fetched. Populated
  // synchronously on a fresh `Generate` (we already have the bytes)
  // and asynchronously per-image on transcript restore. Stored as
  // state so adding an entry triggers a re-render of the lazy
  // <img>s waiting on it.
  const [bytesById, setBytesById] = useState<Map<string, string>>(() => new Map());
  // Most recently submitted prompt, recalled when the user presses ↑
  // in an empty composer. Cleared on submit-then-recall is fine — the
  // user just retypes if they want it back.
  const [lastPrompt, setLastPrompt] = useState<string>("");
  // Sticky model selection across session restarts: hydrated from the
  // last transcript entry on mount, then maintained by the composer.
  // `null` until the transcript load resolves; the composer treats
  // `null` as "fall back to settings.defaultModelId".
  const [stickyModelId, setStickyModelId] = useState<string | null>(null);
  // Open-image lightbox state. The data URL is fetched on demand
  // (same path as the grid thumbnail) so opening a historical image
  // doesn't require pre-loading bytes.
  const [lightbox, setLightbox] = useState<{ imageId: string; alt: string } | null>(null);
  const setImageBytes = useCallback(
    (imageId: string, mime: string, bytesB64: string) => {
      setBytesById((m) => {
        if (m.has(imageId)) return m;
        const next = new Map(m);
        next.set(imageId, `data:${mime};base64,${bytesB64}`);
        return next;
      });
    },
    [],
  );

  // Initial load: pull settings, run a health check, and replay the
  // persisted transcript. All three are independent; transcript
  // arrives separately so a slow disk doesn't gate the empty-state
  // decision.
  useEffect(() => {
    let cancelled = false;
    lutin
      .request(encodeImageRequest({ kind: "getSettings" }))
      .then((b) => {
        if (cancelled) return;
        const r = decodeImageResponse(b);
        if (r.ok && r.value.kind === "settings") setSettings(r.value.settings);
      })
      .catch(() => {});
    runHealthCheck(lutin, setHealth, cancelled);
    lutin
      .request(encodeImageRequest({ kind: "loadTranscript" }))
      .then((b) => {
        if (cancelled) return;
        const r = decodeImageResponse(b);
        if (r.ok && r.value.kind === "transcript") {
          setTurns(r.value.entries.map(transcriptEntryToTurn));
          // Pick up the most recent entry's model so a session
          // resumes with the model the user last picked, not the
          // workflow-wide default.
          for (let i = r.value.entries.length - 1; i >= 0; i--) {
            const id = r.value.entries[i].modelId;
            if (id) {
              setStickyModelId(id);
              break;
            }
          }
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [lutin]);

  // Bind broadcasts to the most-recent pending turn. Only one job is
  // in flight at a time (composer is `disabled` while busy), so the
  // "first jobless pending turn" matches `JobQueued` unambiguously;
  // subsequent events match by `jobId`.
  useEffect(() => {
    const off = lutin.onBroadcast((body) => {
      let ev: ReturnType<typeof decodeImageEvent>;
      try {
        ev = decodeImageEvent(body);
      } catch (err) {
        console.warn("malformed ImageEvent broadcast", err);
        return;
      }
      setTurns((ts) => {
        if (ev.kind === "jobQueued") {
          for (let i = 0; i < ts.length; i++) {
            const t = ts[i];
            if (t.status === "pending" && t.jobId === null) {
              const next = ts.slice();
              next[i] = { ...t, jobId: ev.jobId };
              return next;
            }
          }
          return ts;
        }
        if (ev.kind === "jobProgress") {
          return ts.map((t) =>
            t.jobId === ev.jobId
              ? { ...t, progress: { step: ev.step, total: ev.total } }
              : t,
          );
        }
        return ts;
      });
    });
    return off;
  }, [lutin]);

  const submit = useCallback(
    async (text: string, overrides: DraftOverrides) => {
      if (!text || busy || !settings) return;
      const id = Date.now();
      const turn: Turn = {
        id,
        prompt: text,
        status: "pending",
        images: [],
        error: null,
        jobId: null,
        progress: null,
      };
      setTurns((ts) => [...ts, turn]);
      setLastPrompt(text);
      setBusy(true);
      try {
        // Resolve model first: per-turn override > settings default.
        // Steps/CFG fall back to the per-model recommended defaults
        // (schnell wants 4/1.0, flux2-dev wants 28/3.5) rather than
        // the workflow-wide settings defaults — those are tuned for
        // whatever the user picked as their default model and would
        // be wrong for any other.
        const modelId =
          overrides.modelId ?? stickyModelId ?? settings.defaultModelId;
        const md = MODEL_DEFAULTS[modelId];
        const stepsDefault = md ? md.steps : settings.defaultSteps;
        const cfgDefault = md ? md.cfg : settings.defaultCfg;
        const body = encodeImageRequest({
          kind: "generate",
          params: {
            prompt: text,
            negativePrompt: overrides.negativePrompt,
            seed: overrides.seed,
            width: overrides.width ?? settings.defaultWidth,
            height: overrides.height ?? settings.defaultHeight,
            count: overrides.count ?? settings.defaultCount,
            steps: overrides.steps ?? stepsDefault,
            cfg: overrides.cfg ?? cfgDefault,
            modelId,
          },
        });
        const respBytes = await lutin.request(body);
        const resp = decodeImageResponse(respBytes);
        setTurns((ts) =>
          ts.map((t) => {
            if (t.id !== id) return t;
            if (!resp.ok) {
              return { ...t, status: "error", error: imageErrorMessage(resp.error) };
            }
            if (resp.value.kind === "images") {
              for (const img of resp.value.images) {
                setImageBytes(img.imageId, img.mime, img.bytesB64);
              }
              const refs: TurnImage[] = resp.value.images.map((img) => ({
                imageId: img.imageId,
                mime: img.mime,
                seed: img.seed,
                ms: img.ms,
              }));
              return { ...t, status: "done", images: refs };
            }
            return { ...t, status: "error", error: "unexpected response" };
          }),
        );
      } catch (e) {
        setTurns((ts) =>
          ts.map((t) =>
            t.id === id ? { ...t, status: "error", error: String(e) } : t,
          ),
        );
      } finally {
        setBusy(false);
      }
    },
    [busy, lutin, setImageBytes, settings, stickyModelId],
  );

  // Regenerate a turn's image with the same prompt + seed. Other
  // params reset to settings defaults — we don't currently carry the
  // original turn's `width/height/steps/cfg` on the Turn object, and
  // wiring those just for this action isn't worth the surface.
  const regenerate = useCallback(
    (prompt: string, seed: bigint) => {
      void submit(prompt, { ...EMPTY_OVERRIDES, seed });
    },
    [submit],
  );

  const onSaveSettings = useCallback(
    async (next: ImageSettings) => {
      const respBytes = await lutin.request(
        encodeImageRequest({ kind: "setSettings", settings: next }),
      );
      const resp = decodeImageResponse(respBytes);
      if (!resp.ok) throw new Error(imageErrorMessage(resp.error));
      setSettings(next);
      // URL change might have moved us to a working / broken instance;
      // re-probe so the empty state recovers without a manual refresh.
      runHealthCheck(lutin, setHealth, false);
    },
    [lutin],
  );

  // ComfyUI is the hard prereq; the empty state is the right place to
  // show that until it's reachable. We still let the user open the
  // settings panel from here so they can fix the URL without leaving
  // the iframe.
  if (settings && health.state === "down") {
    return (
      <div className={`lutin-chat ${styles.root}`}>
        <Header onOpenSettings={() => setShowSettings(true)} />
        <div className={styles.unreachable}>
          <div className={styles.unreachableTitle}>ComfyUI not reachable</div>
          <div className={styles.unreachableUrl}>
            <code>{settings.comfyuiUrl}</code>
          </div>
          <div className={styles.unreachableMsg}>{health.message}</div>
          <button
            className={styles.unreachableButton}
            onClick={() => runHealthCheck(lutin, setHealth, false)}
          >
            Retry
          </button>
        </div>
        {showSettings && (
          <SettingsPanel
            initial={settings}
            onClose={() => setShowSettings(false)}
            onSave={onSaveSettings}
          />
        )}
      </div>
    );
  }

  return (
    <div className={`lutin-chat ${styles.root}`}>
      <Header onOpenSettings={() => setShowSettings(true)} />
      <div className={styles.scrollback}>
        {turns.length === 0 && (
          <div className={styles.empty}>Type a prompt below.</div>
        )}
        {turns.map((t) => (
          <TurnView
            key={t.id}
            turn={t}
            lutin={lutin}
            bytesById={bytesById}
            setImageBytes={setImageBytes}
            onOpenImage={(imageId, alt) => setLightbox({ imageId, alt })}
            onRegenerate={regenerate}
          />
        ))}
      </div>
      <Composer
        onSubmit={submit}
        busy={busy}
        settings={settings}
        lastPrompt={lastPrompt}
        stickyModelId={stickyModelId}
        onPickModel={setStickyModelId}
      />
      {lightbox && (
        <Lightbox
          imageId={lightbox.imageId}
          alt={lightbox.alt}
          lutin={lutin}
          bytesById={bytesById}
          setImageBytes={setImageBytes}
          onClose={() => setLightbox(null)}
        />
      )}
      {showSettings && settings && (
        <SettingsPanel
          initial={settings}
          onClose={() => setShowSettings(false)}
          onSave={onSaveSettings}
        />
      )}
    </div>
  );
}

function runHealthCheck(
  lutin: Lutin,
  setHealth: (h: Health) => void,
  cancelledFlag: boolean,
) {
  setHealth({ state: "checking" });
  lutin
    .request(encodeImageRequest({ kind: "healthCheck" }))
    .then((b) => {
      if (cancelledFlag) return;
      const r = decodeImageResponse(b);
      if (r.ok && r.value.kind === "health") {
        setHealth(
          r.value.reachable
            ? { state: "ok" }
            : { state: "down", message: r.value.message },
        );
      }
    })
    .catch((e) => {
      if (cancelledFlag) return;
      setHealth({ state: "down", message: String(e) });
    });
}

function Header({ onOpenSettings }: { onOpenSettings: () => void }) {
  return (
    <div className={styles.header}>
      <div className={styles.headerSpacer} />
      <button
        className={styles.headerBtn}
        onClick={onOpenSettings}
        aria-label="Settings"
        title="Settings"
      >
        ⚙
      </button>
    </div>
  );
}

interface TurnViewProps {
  turn: Turn;
  lutin: Lutin;
  bytesById: Map<string, string>;
  setImageBytes: (imageId: string, mime: string, bytesB64: string) => void;
  onOpenImage: (imageId: string, alt: string) => void;
  onRegenerate: (prompt: string, seed: bigint) => void;
}

function TurnView({
  turn,
  lutin,
  bytesById,
  setImageBytes,
  onOpenImage,
  onRegenerate,
}: TurnViewProps) {
  return (
    <div className={styles.turn}>
      <div className={styles.prompt}>{turn.prompt}</div>
      {turn.status === "pending" && <PendingIndicator turn={turn} />}
      {turn.status === "error" && <div className={styles.error}>{turn.error}</div>}
      {turn.status === "done" && turn.images.length > 0 && (
        <ImageGrid
          prompt={turn.prompt}
          images={turn.images}
          lutin={lutin}
          bytesById={bytesById}
          setImageBytes={setImageBytes}
          onOpenImage={onOpenImage}
          onRegenerate={onRegenerate}
        />
      )}
    </div>
  );
}

interface ImageGridProps {
  prompt: string;
  images: TurnImage[];
  lutin: Lutin;
  bytesById: Map<string, string>;
  setImageBytes: (imageId: string, mime: string, bytesB64: string) => void;
  onOpenImage: (imageId: string, alt: string) => void;
  onRegenerate: (prompt: string, seed: bigint) => void;
}

function ImageGrid({
  prompt,
  images,
  lutin,
  bytesById,
  setImageBytes,
  onOpenImage,
  onRegenerate,
}: ImageGridProps) {
  // 1 image → full width; 2 → side by side; 3-4 → 2x2; ≥5 → 3-col.
  const cols = images.length === 1 ? 1 : images.length <= 4 ? 2 : 3;
  return (
    <div>
      <div className={styles.grid} style={{ gridTemplateColumns: `repeat(${cols}, 1fr)` }}>
        {images.map((img) => (
          <div key={img.imageId} className={styles.gridItem}>
            <LazyImage
              imageId={img.imageId}
              alt={prompt}
              lutin={lutin}
              bytesById={bytesById}
              setImageBytes={setImageBytes}
              onClick={() => onOpenImage(img.imageId, prompt)}
            />
            <ImageActions
              prompt={prompt}
              imageId={img.imageId}
              seed={img.seed}
              onOpen={() => onOpenImage(img.imageId, prompt)}
              onRegenerate={() => onRegenerate(prompt, img.seed)}
            />
          </div>
        ))}
      </div>
      <div className={styles.imageMeta}>
        seed {String(images[0].seed)}
        {images[0].ms > 0 ? ` · ${images[0].ms} ms` : ""}
        {images.length > 1 ? ` · ${images.length} images` : ""}
      </div>
    </div>
  );
}

interface ImageActionsProps {
  prompt: string;
  imageId: string;
  seed: bigint;
  onOpen: () => void;
  onRegenerate: () => void;
}

function ImageActions({
  prompt,
  imageId,
  seed,
  onOpen,
  onRegenerate,
}: ImageActionsProps) {
  // Tiny pill of icon-only actions overlayed on each grid item.
  // Clipboard writes use the iframe's `navigator.clipboard` directly;
  // workflows run in a sandbox but this surface is permitted because
  // it's gated behind a user gesture (click).
  const copy = useCallback(async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // Best-effort: a permission-denied clipboard isn't worth a
      // toast — the user can fall back to opening the image.
    }
  }, []);
  return (
    <div className={styles.imageActions}>
      <button
        type="button"
        className={styles.imageAction}
        onClick={onOpen}
        title="Open"
        aria-label="Open image"
      >
        ⤢
      </button>
      <button
        type="button"
        className={styles.imageAction}
        onClick={() => copy(imageId)}
        title="Copy path"
        aria-label="Copy image path"
      >
        ⧉
      </button>
      <button
        type="button"
        className={styles.imageAction}
        onClick={() => copy(prompt)}
        title="Copy prompt"
        aria-label="Copy prompt"
      >
        ✎
      </button>
      <button
        type="button"
        className={styles.imageAction}
        onClick={onRegenerate}
        title={`Regenerate with seed ${seed}`}
        aria-label="Regenerate with same seed"
      >
        ↻
      </button>
    </div>
  );
}

interface LazyImageProps {
  imageId: string;
  alt: string;
  lutin: Lutin;
  bytesById: Map<string, string>;
  setImageBytes: (imageId: string, mime: string, bytesB64: string) => void;
  onClick?: () => void;
  className?: string;
}

function LazyImage({
  imageId,
  alt,
  lutin,
  bytesById,
  setImageBytes,
  onClick,
  className,
}: LazyImageProps) {
  const src = bytesById.get(imageId);
  // First-mount fetch when bytes aren't already cached. Each image
  // owns its own request — restored sessions fan these out in
  // parallel rather than gating on a single sequential loader.
  useEffect(() => {
    if (src) return;
    let cancelled = false;
    lutin
      .request(encodeImageRequest({ kind: "getImage", imageId }))
      .then((b) => {
        if (cancelled) return;
        const r = decodeImageResponse(b);
        if (r.ok && r.value.kind === "image") {
          const img = r.value.image;
          setImageBytes(img.imageId, img.mime, img.bytesB64);
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [imageId, src, lutin, setImageBytes]);
  if (!src) {
    return <div className={className ?? styles.imagePlaceholder} />;
  }
  return (
    <img
      className={className ?? styles.image}
      src={src}
      alt={alt}
      onClick={onClick}
      style={onClick ? { cursor: "zoom-in" } : undefined}
    />
  );
}

interface LightboxProps {
  imageId: string;
  alt: string;
  lutin: Lutin;
  bytesById: Map<string, string>;
  setImageBytes: (imageId: string, mime: string, bytesB64: string) => void;
  onClose: () => void;
}

function Lightbox({
  imageId,
  alt,
  lutin,
  bytesById,
  setImageBytes,
  onClose,
}: LightboxProps) {
  // Esc closes; bound at the document level because the modal scrim
  // doesn't capture keyboard focus by default.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div className={styles.lightboxScrim} onClick={onClose}>
      <div className={styles.lightboxBody} onClick={(e) => e.stopPropagation()}>
        <LazyImage
          imageId={imageId}
          alt={alt}
          lutin={lutin}
          bytesById={bytesById}
          setImageBytes={setImageBytes}
          className={styles.lightboxImage}
        />
        <button
          type="button"
          className={styles.lightboxClose}
          onClick={onClose}
          aria-label="Close"
        >
          ✕
        </button>
      </div>
    </div>
  );
}

function transcriptEntryToTurn(entry: TranscriptEntry, idx: number): Turn {
  // Stable id derived from started_at + index so historical turns
  // don't collide with `Date.now()`-based ids of fresh generations.
  const id = -(idx + 1);
  if (entry.status.kind === "error") {
    return {
      id,
      prompt: entry.prompt,
      status: "error",
      images: [],
      error: entry.status.message,
      jobId: null,
      progress: null,
    };
  }
  return {
    id,
    prompt: entry.prompt,
    status: "done",
    images: entry.status.images.map((i) => ({
      imageId: i.imageId,
      mime: i.mime,
      seed: i.seed,
      ms: i.ms,
    })),
    error: null,
    jobId: null,
    progress: null,
  };
}

function PendingIndicator({ turn }: { turn: Turn }) {
  if (turn.progress && turn.progress.total > 0) {
    const pct = Math.min(100, Math.round((turn.progress.step / turn.progress.total) * 100));
    return (
      <div className={styles.progress}>
        <div className={styles.progressTrack}>
          <div className={styles.progressFill} style={{ width: `${pct}%` }} />
        </div>
        <div className={styles.progressLabel}>
          step {turn.progress.step}/{turn.progress.total}
        </div>
      </div>
    );
  }
  const label = turn.jobId === null ? "queued…" : "starting…";
  return <div className={styles.status}>{label}</div>;
}

interface ComposerProps {
  onSubmit: (text: string, overrides: DraftOverrides) => void;
  busy: boolean;
  settings: ImageSettings | null;
  /// Last submitted prompt — recalled into the draft on ↑ when the
  /// textarea is empty, mirroring shell history.
  lastPrompt: string;
  /// Last model used in this session (hydrated from transcript on
  /// mount). Acts as a second-tier default between the per-turn
  /// override and the workflow-wide settings default. `null` means
  /// "no preference" — fall through to settings.
  stickyModelId: string | null;
  /// Notify App whenever the user picks a model so the sticky stays
  /// in sync across re-renders within a single session.
  onPickModel: (id: string) => void;
}

const Composer = memo(function Composer({
  onSubmit,
  busy,
  settings,
  lastPrompt,
  stickyModelId,
  onPickModel,
}: ComposerProps) {
  const [draft, setDraft] = useState("");
  const [overrides, setOverrides] = useState<DraftOverrides>(EMPTY_OVERRIDES);
  const [advanced, setAdvanced] = useState(false);

  const fire = () => {
    const text = draft.trim();
    if (!text || busy || !settings) return;
    setDraft("");
    onSubmit(text, overrides);
  };
  const canSubmit = !busy && !!settings && draft.trim().length > 0;
  const modelId =
    overrides.modelId ?? stickyModelId ?? settings?.defaultModelId ?? null;
  const pickModel = useCallback(
    (id: string) => {
      setOverrides((prev) => ({ ...prev, modelId: id }));
      onPickModel(id);
    },
    [onPickModel],
  );
  const toggleAdvanced = useCallback(() => setAdvanced((v) => !v), []);
  return (
    <form
      className={styles.composer}
      onSubmit={(e) => {
        e.preventDefault();
        fire();
      }}
    >
      <div className={styles.composerColumn}>
        <div className={styles.shell} data-disabled={busy || !settings || undefined}>
          <textarea
            className={styles.input}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              // ⌘↵ / Ctrl+↵: submit unconditionally (matches the
              // chat composer). Plain ↵ submits unless shift is held;
              // shift+↵ inserts a newline. ↑ in an empty draft
              // recalls the last submitted prompt.
              if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                e.preventDefault();
                fire();
                return;
              }
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                fire();
                return;
              }
              if (e.key === "ArrowUp" && draft.length === 0 && lastPrompt) {
                e.preventDefault();
                setDraft(lastPrompt);
              }
            }}
            placeholder="Describe an image…"
            rows={1}
            disabled={busy || !settings}
          />
          <ComposerToolbar
            modelId={modelId}
            onPickModel={pickModel}
            advanced={advanced}
            onToggleAdvanced={toggleAdvanced}
            busy={busy}
            canSubmit={canSubmit}
          />
        </div>
        {advanced && settings && (
          <AdvancedFields
            overrides={overrides}
            settings={settings}
            stickyModelId={stickyModelId}
            onChange={setOverrides}
          />
        )}
      </div>
    </form>
  );
});

interface ComposerToolbarProps {
  modelId: string | null;
  onPickModel: (id: string) => void;
  advanced: boolean;
  onToggleAdvanced: () => void;
  busy: boolean;
  canSubmit: boolean;
}

// Memo'd: re-renders only when the toolbar's own props change, so
// keystrokes in the textarea (which only flip `canSubmit` between
// false→true at the boundaries) don't churn the dropdown / advanced
// toggle.
const ComposerToolbar = memo(function ComposerToolbar({
  modelId,
  onPickModel,
  advanced,
  onToggleAdvanced,
  busy,
  canSubmit,
}: ComposerToolbarProps) {
  return (
    <div className={styles.toolbar}>
      {modelId !== null && (
        <Select
          value={modelId}
          options={MODEL_IDS.map((id) => ({
            value: id,
            label: MODEL_DEFAULTS[id]?.label ?? id,
          }))}
          onChange={onPickModel}
          disabled={busy}
          ariaLabel="Model"
        />
      )}
      <button
        type="button"
        className={styles.advancedToggle}
        onClick={onToggleAdvanced}
      >
        {advanced ? "▾" : "▸"} Advanced
      </button>
      <span className={styles.spacer} />
      <span className={styles.hint}>
        {busy ? (
          <span className={styles.streaming}>
            <span className={styles.streamingDot} aria-hidden />
            generating
          </span>
        ) : (
          <>
            <kbd>⏎</kbd>
          </>
        )}
      </span>
      <button
        type="submit"
        className={styles.send}
        disabled={!canSubmit}
        title="Generate (⏎)"
        aria-label="Generate"
      >
        <SendIcon />
      </button>
    </div>
  );
});

interface SelectOption {
  value: string;
  label: string;
}

interface SelectProps {
  value: string;
  options: SelectOption[];
  onChange: (value: string) => void;
  disabled?: boolean;
  ariaLabel?: string;
}

// Custom dropdown — native <select> renders an OS-driven popup that
// gets clipped or misaligned under tiling WMs (i3 in particular).
// Mirrors the persona picker in the chat composer: button + absolutely
// positioned listbox, dismissed on outside pointerdown / Escape.
function Select({ value, options, onChange, disabled, ariaLabel }: SelectProps) {
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (!open) return;
    const onPointer = (e: PointerEvent) => {
      if (!wrapRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("pointerdown", onPointer);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onPointer);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);
  const active = options.find((o) => o.value === value);
  return (
    <div className={styles.selectWrap} ref={wrapRef}>
      <button
        type="button"
        className={styles.selectBtn}
        onClick={() => setOpen((o) => !o)}
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={ariaLabel}
        title={ariaLabel}
      >
        <span className={styles.selectLabel}>{active?.label ?? value}</span>
        <CaretIcon />
      </button>
      {open && (
        <ul role="listbox" className={styles.selectMenu}>
          {options.map((o) => (
            <li
              key={o.value}
              role="option"
              aria-selected={o.value === value}
              className={styles.selectOption}
              data-selected={o.value === value || undefined}
              onPointerDown={(e) => {
                e.preventDefault();
                onChange(o.value);
                setOpen(false);
              }}
            >
              {o.label}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function CaretIcon() {
  return (
    <svg width="9" height="9" viewBox="0 0 10 10" fill="none" aria-hidden>
      <path
        d="M2.5 4l2.5 2.5L7.5 4"
        stroke="currentColor"
        strokeWidth="1.3"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function SendIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" aria-hidden>
      <path
        d="M2.5 8h10M9 4l4 4-4 4"
        stroke="currentColor"
        strokeWidth="1.7"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

interface AdvancedFieldsProps {
  overrides: DraftOverrides;
  settings: ImageSettings;
  stickyModelId: string | null;
  onChange: (next: DraftOverrides) => void;
}

const AdvancedFields = memo(_AdvancedFields);
function _AdvancedFields({
  overrides,
  settings,
  stickyModelId,
  onChange,
}: AdvancedFieldsProps) {
  // Each numeric override falls back to the workflow default when blank.
  // Steps/CFG placeholders track the selected model — the workflow
  // settings carry one default each, but the right number for the
  // chosen graph is what the user actually wants to see.
  const set = <K extends keyof DraftOverrides>(k: K, v: DraftOverrides[K]) =>
    onChange({ ...overrides, [k]: v });
  const modelId =
    overrides.modelId ?? stickyModelId ?? settings.defaultModelId;
  const md = MODEL_DEFAULTS[modelId];
  const stepsPh = md ? md.steps : settings.defaultSteps;
  const cfgPh = md ? md.cfg : settings.defaultCfg;
  return (
    <div className={styles.advanced}>
      <label className={styles.advancedField}>
        <span className={styles.advancedLabel}>Negative prompt</span>
        <input
          className={styles.advancedInput}
          value={overrides.negativePrompt}
          onChange={(e) => set("negativePrompt", e.target.value)}
          placeholder="(none)"
        />
      </label>
      <div className={styles.advancedGrid}>
        <NumField
          label="Count"
          value={overrides.count}
          placeholder={String(settings.defaultCount)}
          min={1}
          max={8}
          onChange={(v) => set("count", v)}
        />
        <NumField
          label="Steps"
          value={overrides.steps}
          placeholder={String(stepsPh)}
          min={1}
          max={150}
          onChange={(v) => set("steps", v)}
        />
        <NumField
          label="CFG"
          value={overrides.cfg}
          placeholder={cfgPh.toString()}
          min={0}
          max={30}
          step={0.1}
          onChange={(v) => set("cfg", v)}
          allowFloat
        />
        <NumField
          label="Width"
          value={overrides.width}
          placeholder={String(settings.defaultWidth)}
          min={64}
          max={4096}
          step={64}
          onChange={(v) => set("width", v)}
        />
        <NumField
          label="Height"
          value={overrides.height}
          placeholder={String(settings.defaultHeight)}
          min={64}
          max={4096}
          step={64}
          onChange={(v) => set("height", v)}
        />
        <SeedField
          value={overrides.seed}
          onChange={(v) => set("seed", v)}
        />
      </div>
    </div>
  );
}

interface NumFieldProps {
  label: string;
  value: number | null;
  placeholder: string;
  min?: number;
  max?: number;
  step?: number;
  allowFloat?: boolean;
  onChange: (v: number | null) => void;
}

function NumField({
  label,
  value,
  placeholder,
  min,
  max,
  step,
  allowFloat,
  onChange,
}: NumFieldProps) {
  return (
    <label className={styles.advancedField}>
      <span className={styles.advancedLabel}>{label}</span>
      <input
        className={styles.advancedInput}
        type="number"
        inputMode={allowFloat ? "decimal" : "numeric"}
        value={value ?? ""}
        placeholder={placeholder}
        min={min}
        max={max}
        step={step}
        onChange={(e) => {
          const s = e.target.value;
          if (s === "") {
            onChange(null);
            return;
          }
          const n = allowFloat ? Number(s) : parseInt(s, 10);
          if (Number.isFinite(n)) onChange(n);
        }}
      />
    </label>
  );
}

function SeedField({
  value,
  onChange,
}: {
  value: bigint | null;
  onChange: (v: bigint | null) => void;
}) {
  // Seed is u64. Browser <input type=number> chokes on values >
  // 2^53, so we use a text input and parse explicitly. Empty = random.
  return (
    <label className={styles.advancedField}>
      <span className={styles.advancedLabel}>Seed</span>
      <input
        className={styles.advancedInput}
        type="text"
        inputMode="numeric"
        value={value === null ? "" : value.toString()}
        placeholder="(random)"
        onChange={(e) => {
          const s = e.target.value.trim();
          if (s === "") {
            onChange(null);
            return;
          }
          try {
            const n = BigInt(s);
            if (n >= 0n) onChange(n);
          } catch {
            // Ignore non-numeric input; the field stays at its last
            // valid value rather than blowing up.
          }
        }}
      />
    </label>
  );
}

interface SettingsPanelProps {
  initial: ImageSettings;
  onClose: () => void;
  onSave: (next: ImageSettings) => Promise<void>;
}

function SettingsPanel({ initial, onClose, onSave }: SettingsPanelProps) {
  const [draft, setDraft] = useState<ImageSettings>(initial);
  const [saving, setSaving] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const set = <K extends keyof ImageSettings>(k: K, v: ImageSettings[K]) =>
    setDraft((d) => ({ ...d, [k]: v }));
  const save = async () => {
    setSaving(true);
    setErr(null);
    try {
      await onSave(draft);
      onClose();
    } catch (e) {
      setErr(String(e));
    } finally {
      setSaving(false);
    }
  };
  return (
    <div className={styles.modalScrim} onClick={onClose}>
      <div className={styles.modal} onClick={(e) => e.stopPropagation()}>
        <div className={styles.modalHeader}>
          <div className={styles.modalTitle}>Image settings</div>
          <button className={styles.modalClose} onClick={onClose} aria-label="Close">
            ✕
          </button>
        </div>
        <div className={styles.modalBody}>
          <label className={styles.advancedField}>
            <span className={styles.advancedLabel}>ComfyUI URL</span>
            <input
              className={styles.advancedInput}
              value={draft.comfyuiUrl}
              onChange={(e) => set("comfyuiUrl", e.target.value)}
              placeholder="http://127.0.0.1:8188"
            />
          </label>
          <label className={styles.advancedField}>
            <span className={styles.advancedLabel}>Default model</span>
            <Select
              value={draft.defaultModelId}
              options={MODEL_IDS.map((id) => ({
                value: id,
                label: MODEL_DEFAULTS[id]?.label ?? id,
              }))}
              onChange={(v) => set("defaultModelId", v)}
              ariaLabel="Default model"
            />
          </label>
          <div className={styles.advancedGrid}>
            <SettingsNum
              label="Default width"
              value={draft.defaultWidth}
              onChange={(v) => set("defaultWidth", v)}
            />
            <SettingsNum
              label="Default height"
              value={draft.defaultHeight}
              onChange={(v) => set("defaultHeight", v)}
            />
            <SettingsNum
              label="Default count"
              value={draft.defaultCount}
              onChange={(v) => set("defaultCount", v)}
            />
            <SettingsNum
              label="Default steps"
              value={draft.defaultSteps}
              onChange={(v) => set("defaultSteps", v)}
            />
            <SettingsNum
              label="Default CFG"
              value={draft.defaultCfg}
              float
              onChange={(v) => set("defaultCfg", v)}
            />
          </div>
          {err && <div className={styles.error}>{err}</div>}
        </div>
        <div className={styles.modalFooter}>
          <button className={styles.modalCancel} onClick={onClose} disabled={saving}>
            Cancel
          </button>
          <button className={styles.modalSave} onClick={save} disabled={saving}>
            {saving ? "Saving…" : "Save"}
          </button>
        </div>
      </div>
    </div>
  );
}

function SettingsNum({
  label,
  value,
  float,
  onChange,
}: {
  label: string;
  value: number;
  float?: boolean;
  onChange: (v: number) => void;
}) {
  return (
    <label className={styles.advancedField}>
      <span className={styles.advancedLabel}>{label}</span>
      <input
        className={styles.advancedInput}
        type="number"
        inputMode={float ? "decimal" : "numeric"}
        value={value}
        onChange={(e) => {
          const s = e.target.value;
          if (s === "") return;
          const n = float ? Number(s) : parseInt(s, 10);
          if (Number.isFinite(n)) onChange(n);
        }}
      />
    </label>
  );
}
