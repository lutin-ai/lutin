// App-level (in-process) keybind layer. Distinct from the Rust-side
// global shortcuts in `DesktopSettings.keybinds` — those fire from the
// OS even when the app isn't focused; these only fire when the
// chrome's window has focus and the event target isn't a text input.
//
// Combo grammar:
//   - single key:           "i", "p", "?"
//   - leader + key:         "space p"  (press space, release, then p)
//   - modifier + key:       "ctrl k", "ctrl shift p"  (concurrent)
// Leader chords cannot combine with modifiers in v1 — modifier chords
// don't need a leader because the modifier is the discoverable prefix.
//
// Persistence: localStorage. App-only state, no need to round-trip
// through Rust. Promote to DesktopSettings later if cross-machine sync
// is wanted.

import { useEffect } from "react";
import { create } from "zustand";

export type AppAction =
  | "openProjectPicker"
  | "openSessionPicker"
  | "openNewSessionPicker"
  | "openQuickChat"
  | "focusWorkflow";

export interface AppKeybind {
  action: AppAction;
  combo: string;
}

export const APP_ACTION_LABELS: Record<AppAction, string> = {
  openProjectPicker: "Open project picker",
  openSessionPicker: "Open session picker (current project)",
  openNewSessionPicker: "Start a new session (current project)",
  openQuickChat: "Open quick chat",
  focusWorkflow: "Focus workflow / chat composer",
};

const DEFAULTS: AppKeybind[] = [
  { action: "openProjectPicker", combo: "space p" },
  { action: "openSessionPicker", combo: "space f" },
  { action: "openNewSessionPicker", combo: "space n" },
  { action: "openQuickChat", combo: "space q" },
  { action: "focusWorkflow", combo: "i" },
];

const STORAGE_KEY = "lutin.appKeybinds.v1";

function loadBinds(): AppKeybind[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return DEFAULTS;
    const parsed = JSON.parse(raw) as AppKeybind[];
    // Backfill any missing default actions so a stored set from a
    // previous build doesn't lock the user out of newer actions.
    const have = new Set(parsed.map((b) => b.action));
    const merged = [...parsed];
    for (const def of DEFAULTS) if (!have.has(def.action)) merged.push(def);
    return merged;
  } catch {
    return DEFAULTS;
  }
}

function saveBinds(binds: AppKeybind[]) {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(binds));
}

interface KeybindsState {
  binds: AppKeybind[];
  /// Name of the leader key currently held (e.g. "space"), or null
  /// when no chord is in flight. Drives the in-app which-key overlay.
  pendingLeader: string | null;
  setBind: (action: AppAction, combo: string) => void;
  reset: () => void;
  setPendingLeader: (leader: string | null) => void;
}

export const useAppKeybinds = create<KeybindsState>((set) => ({
  binds: loadBinds(),
  pendingLeader: null,
  setBind: (action, combo) =>
    set((s) => {
      const next = s.binds.map((b) => (b.action === action ? { ...b, combo } : b));
      saveBinds(next);
      return { binds: next };
    }),
  reset: () => {
    saveBinds(DEFAULTS);
    set({ binds: DEFAULTS });
  },
  setPendingLeader: (leader) => set({ pendingLeader: leader }),
}));

// Parsed combo: either modifier-style (one non-modifier key + bitmask)
// or leader-style (leader key, then one key with no modifiers).
type ParsedCombo =
  | { kind: "single"; key: string; ctrl: boolean; meta: boolean; shift: boolean; alt: boolean }
  | { kind: "leader"; leader: string; key: string };

function parseCombo(combo: string): ParsedCombo | null {
  const parts = combo.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (parts.length === 0) return null;
  // Leader form: first token is "space" (or future leaders) and there
  // is no modifier token. Modifier form: tokens may include
  // ctrl/cmd/shift/alt plus exactly one regular key at the end.
  const MODS = new Set(["ctrl", "control", "cmd", "meta", "shift", "alt", "option"]);
  if (parts[0] === "space" && parts.length >= 2 && !MODS.has(parts[1])) {
    if (parts.length !== 2) return null;
    return { kind: "leader", leader: "space", key: parts[1] };
  }
  let ctrl = false, meta = false, shift = false, alt = false;
  let key: string | null = null;
  for (const p of parts) {
    if (p === "ctrl" || p === "control") ctrl = true;
    else if (p === "cmd" || p === "meta") meta = true;
    else if (p === "shift") shift = true;
    else if (p === "alt" || p === "option") alt = true;
    else {
      if (key !== null) return null;
      key = p;
    }
  }
  if (!key) return null;
  return { kind: "single", key, ctrl, meta, shift, alt };
}

function eventKey(e: KeyboardEvent): string {
  // Normalize space + alphanumerics to a single token. We use `key`
  // rather than `code` so that the user's typed combo ("space p")
  // matches what they see, not a US-layout-specific scancode.
  const k = e.key;
  if (k === " ") return "space";
  if (k === "Escape") return "escape";
  if (k.length === 1) return k.toLowerCase();
  return k.toLowerCase();
}

function isTextTarget(el: EventTarget | null): boolean {
  if (!(el instanceof HTMLElement)) return false;
  const tag = el.tagName;
  if (tag === "INPUT" || tag === "TEXTAREA") return true;
  if (el.isContentEditable) return true;
  return false;
}

export interface AppActionHandlers {
  openProjectPicker: () => void;
  openSessionPicker: () => void;
  openNewSessionPicker: () => void;
  openQuickChat: () => void;
  focusWorkflow: () => void;
}

/// Mount once at the app shell. Listens at the document level, skips
/// while a text input has focus, and dispatches matched combos.
export function useAppKeybindDispatch(handlers: AppActionHandlers) {
  const binds = useAppKeybinds((s) => s.binds);
  const setPendingLeader = useAppKeybinds((s) => s.setPendingLeader);

  useEffect(() => {
    const parsed = binds
      .map((b) => ({ action: b.action, parsed: parseCombo(b.combo) }))
      .filter((b) => b.parsed !== null) as { action: AppAction; parsed: ParsedCombo }[];

    let pendingLeader: string | null = null;
    let leaderTimer: number | null = null;

    const clearLeader = () => {
      pendingLeader = null;
      setPendingLeader(null);
      if (leaderTimer !== null) {
        window.clearTimeout(leaderTimer);
        leaderTimer = null;
      }
    };

    const fire = (action: AppAction) => {
      switch (action) {
        case "openProjectPicker": handlers.openProjectPicker(); break;
        case "openSessionPicker": handlers.openSessionPicker(); break;
        case "openNewSessionPicker": handlers.openNewSessionPicker(); break;
        case "openQuickChat": handlers.openQuickChat(); break;
        case "focusWorkflow": handlers.focusWorkflow(); break;
      }
    };

    const onKey = (e: KeyboardEvent) => {
      // While typing in a text input, no app keybinds fire. The user
      // explicitly opts back into nav by blurring (Esc) — which still
      // works because Esc doesn't go through this layer.
      if (isTextTarget(e.target)) return;
      if (e.altKey && e.key === "Tab") return;
      const key = eventKey(e);

      if (key === "escape") {
        if (pendingLeader) {
          e.preventDefault();
          clearLeader();
        }
        return;
      }

      // In-leader: only the key part is needed; modifiers cancel.
      if (pendingLeader) {
        if (e.ctrlKey || e.metaKey || e.altKey) {
          clearLeader();
          return;
        }
        const leader = pendingLeader;
        clearLeader();
        const hit = parsed.find(
          (b) => b.parsed.kind === "leader" && b.parsed.leader === leader && b.parsed.key === key,
        );
        if (hit) {
          e.preventDefault();
          fire(hit.action);
        }
        return;
      }

      // Out-of-leader: either start a leader, or match a single/modifier combo.
      const leaderStart = parsed.find(
        (b) => b.parsed.kind === "leader" && b.parsed.leader === key && !e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey,
      );
      if (leaderStart) {
        e.preventDefault();
        pendingLeader = key;
        setPendingLeader(key);
        leaderTimer = window.setTimeout(clearLeader, 1500);
        return;
      }

      const single = parsed.find((b) => {
        if (b.parsed.kind !== "single") return false;
        const p = b.parsed;
        return (
          p.key === key &&
          p.ctrl === e.ctrlKey &&
          p.meta === e.metaKey &&
          p.shift === e.shiftKey &&
          p.alt === e.altKey
        );
      });
      if (single) {
        e.preventDefault();
        fire(single.action);
      }
    };

    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
      clearLeader();
    };
  }, [binds, handlers]);
}
