import { useEffect, useRef, useState } from "react";
import type { PersonaInfo } from "@lutin/scratchpad-protocol";
import styles from "./PersonaComposer.module.css";

interface Props {
  value: string;
  onChange: (v: string) => void;
  onSubmit: () => void;
  onCancel?: () => void;
  busy: boolean;
  disabled?: boolean;
  placeholder?: string;
  personas: PersonaInfo[] | null;
  activePersona: string | null;
  onChangePersona: (name: string | null) => void;
}

export function PersonaComposer({
  value,
  onChange,
  onSubmit,
  onCancel,
  busy,
  disabled = false,
  placeholder = "Ask the agent…",
  personas,
  activePersona,
  onChangePersona,
}: Props) {
  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      if (value.trim().length > 0 && !disabled) onSubmit();
    }
  };

  const active = personas?.find((p) => p.name === activePersona) ?? null;
  const modelLabel = active?.model || "";
  const canSubmit = !disabled && value.trim().length > 0;

  return (
    <div className={styles.outer}>
      <div className={styles.shell} data-disabled={disabled || undefined}>
        <textarea
          className={styles.input}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder={placeholder}
          disabled={disabled}
          rows={1}
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
                streaming
              </span>
            ) : (
              <>
                <kbd>⏎</kbd>
              </>
            )}
          </span>
          {busy && onCancel ? (
            <button
              type="button"
              className={styles.send}
              onClick={onCancel}
              title="Stop"
              aria-label="Stop"
            >
              <StopIcon />
            </button>
          ) : (
            <button
              type="button"
              className={styles.send}
              onClick={onSubmit}
              disabled={!canSubmit}
              title="Send (Enter)"
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

function StopIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 16 16" fill="none" aria-hidden>
      <rect x="3.5" y="3.5" width="9" height="9" rx="1.2" fill="currentColor" />
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
