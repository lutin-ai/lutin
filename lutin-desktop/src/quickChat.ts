// Persistent "quick chat" pointer — `space q` jumps here. Stores a
// default project (the one a brand-new quick chat is created in) and
// a pointer to the actively-pinned session. On reuse we verify the
// session still exists; if it was deleted or the project is gone we
// re-create.
//
// Persistence: localStorage. App-only state, no need to round-trip
// through Rust.

import { create } from "zustand";

const STORAGE_KEY = "lutin.quickChat.v1";

export const QUICK_CHAT_WORKFLOW = "chat";

interface Persisted {
  defaultProject: string | null;
  sessionPtr: { project: string; session: string } | null;
}

function load(): Persisted {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { defaultProject: null, sessionPtr: null };
    const parsed = JSON.parse(raw) as Partial<Persisted>;
    return {
      defaultProject: parsed.defaultProject ?? null,
      sessionPtr: parsed.sessionPtr ?? null,
    };
  } catch {
    return { defaultProject: null, sessionPtr: null };
  }
}

function save(p: Persisted) {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(p));
}

interface QuickChatState extends Persisted {
  setDefaultProject: (slug: string | null) => void;
  setSessionPtr: (ptr: { project: string; session: string } | null) => void;
}

export const useQuickChat = create<QuickChatState>((set) => {
  const initial = load();
  return {
    ...initial,
    setDefaultProject: (slug) =>
      set((s) => {
        const next = { ...s, defaultProject: slug };
        save({ defaultProject: next.defaultProject, sessionPtr: next.sessionPtr });
        return next;
      }),
    setSessionPtr: (ptr) =>
      set((s) => {
        const next = { ...s, sessionPtr: ptr };
        save({ defaultProject: next.defaultProject, sessionPtr: next.sessionPtr });
        return next;
      }),
  };
});
