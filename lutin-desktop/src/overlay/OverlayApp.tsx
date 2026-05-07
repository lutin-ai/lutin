import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import styles from "./overlay.module.css";

/// Phases sent over the `overlay:phase` Tauri event from `dispatch.rs`.
/// Mirrors the Rust `OverlayPhase` enum 1:1 — externally tagged on
/// `kind` because the `error` variant carries a message.
type OverlayPhase =
  | { kind: "listening" }
  | { kind: "transcribing" }
  | { kind: "done" }
  | { kind: "error"; message: string };

const PHASE_LABEL: Record<OverlayPhase["kind"], string> = {
  listening: "Listening",
  transcribing: "Transcribing",
  done: "Copied",
  error: "Error",
};

const PHASE_DOT: Record<OverlayPhase["kind"], string> = {
  listening: "listening",
  transcribing: "transcribing",
  done: "done",
  error: "error",
};

export function OverlayApp() {
  const [phase, setPhase] = useState<OverlayPhase | null>(null);

  useEffect(() => {
    // Poll the cached phase from Rust. We tried `listen("overlay:phase")`
    // first but events targeted at this secondary webview were
    // unreliably delivered when the window boots from `visible: false`
    // — late phases (Done) routinely got dropped, leaving "Transcribing"
    // stuck on screen. A 100ms poll on a small indicator window is
    // cheap (one IPC + a Mutex read) and is robust against any IPC
    // ordering quirk.
    let alive = true;
    const tick = async () => {
      try {
        const p = await invoke<OverlayPhase | null>("overlay_current_phase");
        if (alive) setPhase(p);
      } catch {
        /* webview tearing down */
      }
    };
    tick();
    const id = window.setInterval(tick, 100);
    return () => {
      alive = false;
      window.clearInterval(id);
    };
  }, []);

  // Default to `listening` when the cache fetch is still in flight.
  // Rust always shows the window in the `Listening` phase first, so
  // this matches the actual state during the brief window before the
  // phase event reaches us — nothing weird flashes on screen.
  const effective: OverlayPhase = phase ?? { kind: "listening" };
  const label = effective.kind === "error" ? effective.message : PHASE_LABEL[effective.kind];
  const dot = PHASE_DOT[effective.kind];

  return (
    <div className={styles.pill} data-dot={dot} key={effective.kind}>
      {effective.kind === "done" ? (
        <svg
          className={styles.check}
          viewBox="0 0 24 24"
          width="14"
          height="14"
          aria-hidden="true"
        >
          <path
            d="M4 12.5l5 5L20 6.5"
            fill="none"
            stroke="currentColor"
            strokeWidth="3"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      ) : effective.kind === "transcribing" ? (
        <span className={styles.spinner} aria-hidden="true" />
      ) : (
        <span className={styles.dot} />
      )}
      <span className={styles.label}>{label}</span>
    </div>
  );
}
