// Composer for the reviewed workflow: a centered, padded pill with a
// persona picker + model chip, modelled after the input bar in the
// other workflows (principled/scratchpad). The reviewed engine has no
// TTS / rerun / metrics, so this is a trimmed variant — no usage
// footer, no extra tool buttons.

import { useEffect, useRef, useState } from "react";
import type { ComposerProps } from "@lutin/chat-widgets";
import type { PersonaInfo } from "@lutin/reviewed-protocol";
import styles from "./PersonaComposer.module.css";

export interface PersonaComposerExtra {
  personas: PersonaInfo[] | null;
  activePersona: string | null;
  onChangePersona: (name: string | null) => void;
}

// Closure factory: returns a component satisfying the `<ChatView>`
// Composer slot contract while carrying the reviewed-only persona state.
export function makePersonaComposer(extra: PersonaComposerExtra) {
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
  placeholder = "Send a message (⌘⏎)",
  disabled = false,
  personas,
  activePersona,
  onChangePersona,
}: InnerProps) {
  // Auto-grow handled by CSS `field-sizing: content` on `.input`.
  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // Ctrl/Cmd+Enter submits; bare Enter inserts a newline. Matches the
    // other workflows — multi-line drafts are common enough that we
    // don't want Enter to send by accident.
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      if (value.trim().length > 0) onSubmit();
    }
  };

  const active = personas?.find((p) => p.name === activePersona) ?? null;
  const modelLabel = active?.model || "";
  const canSubmit = value.trim().length > 0;

  return (
    <div className={styles.outer}>
      <div className={styles.shell}>
        <textarea
          className={styles.input}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder={busy ? "Working…" : placeholder}
          disabled={disabled}
          rows={1}
          // WebKitGTK stalls input events on spellcheck without a
          // configured dictionary; chat input doesn't need it.
          spellCheck={false}
          autoCorrect="off"
          autoCapitalize="off"
          autoComplete="off"
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
                working
              </span>
            ) : (
              <>
                <kbd>⌘</kbd>
                <kbd>⏎</kbd>
              </>
            )}
          </span>
          {busy ? (
            <button
              type="button"
              className={styles.stop}
              onClick={() => onCancel?.()}
              title="Cancel the current turn"
              aria-label="Cancel"
            >
              <StopIcon />
            </button>
          ) : (
            <button
              type="button"
              className={styles.send}
              onClick={onSubmit}
              disabled={!canSubmit || disabled}
              title="Send (⌘⏎)"
              aria-label="Send"
            >
              <SendIcon />
            </button>
          )}
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
  // Hooks run unconditionally — `personas` flips from `null` to a list
  // once `ListPersonas` lands, and skipping these on the null/empty
  // paths would corrupt hook ordering across renders.
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
  const items: { value: string; label: string }[] = [
    { value: "", label: "No persona" },
    ...personas.map((p) => ({ value: p.name, label: p.displayName })),
  ];
  return (
    <div className={styles.personaWrap} ref={wrapRef}>
      <button
        type="button"
        className={styles.persona}
        data-active={active ? "true" : undefined}
        title="Persona"
        onClick={() => setOpen((o) => !o)}
        aria-haspopup="listbox"
        aria-expanded={open}
      >
        <PersonaIcon />
        <span className={styles.personaLabel}>
          {active?.displayName ?? "No persona"}
        </span>
        <CaretIcon />
      </button>
      {open && (
        <ul role="listbox" className={styles.personaMenu}>
          {items.map((it) => (
            <li
              key={it.value || "__none__"}
              role="option"
              aria-selected={(activePersona ?? "") === it.value}
              className={styles.personaOption}
              data-selected={(activePersona ?? "") === it.value || undefined}
              onPointerDown={(e) => {
                e.preventDefault();
                onChange(it.value || null);
                setOpen(false);
              }}
            >
              {it.label}
            </li>
          ))}
        </ul>
      )}
    </div>
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
