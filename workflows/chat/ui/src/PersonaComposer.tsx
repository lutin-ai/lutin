// Custom Composer with a persona picker + model display, modelled
// after the input bar in the legacy egui desktop. Slots into
// `<ChatView>` via `slots.Composer`.

import { useEffect, useRef } from "react";
import type { ComposerProps } from "@lutin/chat-widgets";
import type { PersonaInfo } from "./chat";
import styles from "./PersonaComposer.module.css";

export interface PersonaComposerExtra {
  personas: PersonaInfo[] | null;
  activePersona: string | null;
  onChangePersona: (name: string | null) => void;
  /** Run the agent loop against the existing transcript without
   * appending a new user message. */
  onRerun: () => void;
  /** TTS toggle. `available` is `false` when chrome didn't expose
   * `lutin.tts` (capability missing); the button is hidden then. */
  ttsAvailable: boolean;
  ttsOn: boolean;
  ttsLoading: boolean;
  onToggleTts: () => void;
}

export function makePersonaComposer(extra: PersonaComposerExtra) {
  // Closure trick: returns a component that satisfies ComposerProps
  // (the slot contract) while having access to the workflow-specific
  // persona state. Re-created when `extra` changes.
  return function PersonaComposer(props: ComposerProps) {
    return <Inner {...props} {...extra} />;
  };
}

type InnerProps = ComposerProps & PersonaComposerExtra;

function Inner({
  value,
  onChange,
  onSubmit,
  onCancel,
  busy,
  placeholder = "Send a message…",
  disabled = false,
  personas,
  activePersona,
  onChangePersona,
  onRerun,
  ttsAvailable,
  ttsOn,
  ttsLoading,
  onToggleTts,
}: InnerProps) {
  const ref = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 240)}px`;
  }, [value]);

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // Ctrl/Cmd+Enter submits; bare Enter inserts a newline. Matches
    // the legacy egui input — multi-line drafts are common enough
    // that we don't want Enter to send by accident. Submission stays
    // available while streaming; the parent queues it.
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      if (value.trim().length > 0) onSubmit();
    }
  };

  const active = personas?.find((p) => p.name === activePersona) ?? null;
  const modelLabel = active?.model || "";
  const canSubmit = !disabled && value.trim().length > 0;

  return (
    <div className={styles.outer}>
      <div className={styles.shell} data-disabled={disabled || undefined}>
        <textarea
          ref={ref}
          className={styles.input}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder={placeholder}
          disabled={disabled}
          rows={1}
        />
        <div className={styles.toolbar}>
          <PersonaPicker
            personas={personas}
            activePersona={activePersona}
            onChange={onChangePersona}
          />
          {modelLabel && (
            <span className={styles.model} title={`model: ${modelLabel}`}>
              <ChipIcon />
              {modelLabel}
            </span>
          )}
          <span className={styles.spacer} />
          <span className={styles.hint}>
            {busy ? (
              <span className={styles.streaming}>
                <span className={styles.streamingDot} aria-hidden />
                streaming
              </span>
            ) : (
              <>
                <kbd>⌘</kbd>
                <kbd>⏎</kbd>
              </>
            )}
          </span>
          <button
            type="button"
            className={styles.toolBtn}
            title="Compact transcript"
            aria-label="Compact"
          >
            <CompactIcon />
            <span>Compact</span>
          </button>
          {ttsAvailable && (
            <button
              type="button"
              className={styles.toolBtn}
              onClick={onToggleTts}
              disabled={ttsLoading}
              title={
                ttsLoading
                  ? "Loading TTS model…"
                  : ttsOn
                    ? "Disable spoken responses"
                    : "Speak assistant replies"
              }
              aria-label="Toggle TTS"
              aria-pressed={ttsOn}
              data-active={ttsOn ? "true" : undefined}
            >
              <SpeakerIcon muted={!ttsOn} />
              <span>{ttsLoading ? "TTS…" : ttsOn ? "TTS on" : "TTS"}</span>
            </button>
          )}
          <button
            type="button"
            className={styles.toolBtn}
            onClick={onRerun}
            disabled={disabled}
            title="Rerun the agent without sending a new message"
            aria-label="Rerun"
          >
            <RerunIcon />
            <span>Rerun</span>
          </button>
          {busy && onCancel && (
            <button
              type="button"
              className={styles.stop}
              onClick={onCancel}
              title="Stop streaming"
              aria-label="Stop"
            >
              <StopIcon />
            </button>
          )}
          <button
            type="button"
            className={styles.send}
            onClick={onSubmit}
            disabled={!canSubmit}
            title={busy ? "Queue message — sends after the current turn (⌘⏎)" : "Send (⌘⏎)"}
            aria-label={busy ? "Queue" : "Send"}
          >
            <SendIcon />
          </button>
        </div>
      </div>
    </div>
  );
}

interface PickerProps {
  personas: PersonaInfo[] | null;
  activePersona: string | null;
  onChange: (name: string | null) => void;
}

function PersonaPicker({ personas, activePersona, onChange }: PickerProps) {
  if (!personas) {
    return (
      <span className={styles.persona} data-state="loading">
        <PersonaIcon />
        loading…
      </span>
    );
  }
  if (personas.length === 0) {
    return (
      <span className={styles.persona} data-state="empty">
        <PersonaIcon />
        no personas
      </span>
    );
  }
  const active = personas.find((p) => p.name === activePersona);
  return (
    <label
      className={styles.persona}
      data-active={active ? "true" : undefined}
      title="Persona"
    >
      <PersonaIcon />
      <select
        value={activePersona ?? ""}
        onChange={(e) => onChange(e.target.value || null)}
      >
        <option value="">No persona</option>
        {personas.map((p) => (
          <option key={p.name} value={p.name}>
            {p.displayName}
          </option>
        ))}
      </select>
      <CaretIcon />
    </label>
  );
}

/* ───────── icons ───────── */

function PersonaIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 14 14" fill="none" aria-hidden>
      <circle cx="7" cy="5" r="2.4" stroke="currentColor" strokeWidth="1.3" />
      <path
        d="M2.5 12.2c.5-2.2 2.4-3.5 4.5-3.5s4 1.3 4.5 3.5"
        stroke="currentColor"
        strokeWidth="1.3"
        strokeLinecap="round"
      />
    </svg>
  );
}

function ChipIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 14 14" fill="none" aria-hidden>
      <rect x="3" y="3" width="8" height="8" rx="1.5" stroke="currentColor" strokeWidth="1.2" />
      <path
        d="M5.5 6h3M5.5 8h3M2 5.5h1M2 8.5h1M11 5.5h1M11 8.5h1M5.5 2v1M8.5 2v1M5.5 11v1M8.5 11v1"
        stroke="currentColor"
        strokeWidth="1.1"
        strokeLinecap="round"
      />
    </svg>
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

function StopIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 12 12" fill="none" aria-hidden>
      <rect x="2.5" y="2.5" width="7" height="7" rx="1" fill="currentColor" />
    </svg>
  );
}

function CompactIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 14 14" fill="none" aria-hidden>
      <path
        d="M3 4h8M3 7h8M3 10h5"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
      />
      <path
        d="M11 9.5l1.5 1.5M11 11l1.5-1.5"
        stroke="currentColor"
        strokeWidth="1.2"
        strokeLinecap="round"
      />
    </svg>
  );
}

function SpeakerIcon({ muted }: { muted: boolean }) {
  return (
    <svg width="13" height="13" viewBox="0 0 14 14" fill="none" aria-hidden>
      <path
        d="M3 5h2l3-2.5v9L5 9H3V5z"
        stroke="currentColor"
        strokeWidth="1.3"
        strokeLinejoin="round"
      />
      {muted ? (
        <path
          d="M10 5l3 4M13 5l-3 4"
          stroke="currentColor"
          strokeWidth="1.3"
          strokeLinecap="round"
        />
      ) : (
        <path
          d="M10 4.5a3.5 3.5 0 0 1 0 5M11.5 3a5.5 5.5 0 0 1 0 8"
          stroke="currentColor"
          strokeWidth="1.3"
          strokeLinecap="round"
        />
      )}
    </svg>
  );
}

function RerunIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 14 14" fill="none" aria-hidden>
      <path
        d="M11.5 7a4.5 4.5 0 1 1-1.32-3.18M11.5 2.2v2.6h-2.6"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}
