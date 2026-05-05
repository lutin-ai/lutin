import { create } from "zustand";
import type {
  ConnState,
  CpEvent,
  ProjectInfo,
  SessionInfo,
  Slug,
  SessionId,
  DesktopSettings,
} from "./types";

// Re-export so older imports keep working; the canonical home is now
// `./types` so the Rust `ConnSnapshot` and the JS shape stay in sync.
export type { ConnState };

export interface AppView {
  kind: "settings" | "project";
}

interface AppState {
  conn: ConnState;
  view: AppView;
  settings: DesktopSettings | null;
  projects: ProjectInfo[];
  selectedProject: Slug | null;
  sessionsBySlug: Record<Slug, SessionInfo[]>;
  selectedSession: SessionId | null;

  setConn: (c: ConnState) => void;
  setView: (v: AppView) => void;
  setSettings: (s: DesktopSettings) => void;
  setProjects: (p: ProjectInfo[]) => void;
  selectProject: (s: Slug | null) => void;
  setSessions: (slug: Slug, sessions: SessionInfo[]) => void;
  selectSession: (id: SessionId | null) => void;
  applyEvent: (e: CpEvent) => void;
}

export const useApp = create<AppState>((set) => ({
  conn: { kind: "connecting" },
  view: { kind: "project" },
  settings: null,
  projects: [],
  selectedProject: null,
  sessionsBySlug: {},
  selectedSession: null,

  setConn: (conn) => set({ conn }),
  setView: (view) => set({ view }),
  setSettings: (settings) => set({ settings }),
  setProjects: (projects) => set({ projects }),
  selectProject: (slug) => set({ selectedProject: slug, selectedSession: null }),
  setSessions: (slug, sessions) =>
    set((s) => ({ sessionsBySlug: { ...s.sessionsBySlug, [slug]: sessions } })),
  selectSession: (id) => set({ selectedSession: id }),

  applyEvent: (event) =>
    set((s) => {
      if ("ProjectCreated" in event) {
        const exists = s.projects.some((p) => p.slug === event.ProjectCreated.slug);
        return {
          projects: exists ? s.projects : [...s.projects, event.ProjectCreated],
        };
      }
      if ("ProjectDeleted" in event) {
        const slug = event.ProjectDeleted.slug;
        const { [slug]: _drop, ...rest } = s.sessionsBySlug;
        return {
          projects: s.projects.filter((p) => p.slug !== slug),
          sessionsBySlug: rest,
          selectedProject: s.selectedProject === slug ? null : s.selectedProject,
        };
      }
      if ("SessionStarted" in event) {
        const { slug, info } = event.SessionStarted;
        const list = s.sessionsBySlug[slug] ?? [];
        if (list.some((x) => x.id === info.id)) return s;
        return {
          sessionsBySlug: { ...s.sessionsBySlug, [slug]: [...list, info] },
        };
      }
      if ("SessionEnded" in event) {
        const { slug, session } = event.SessionEnded;
        const list = s.sessionsBySlug[slug] ?? [];
        return {
          sessionsBySlug: {
            ...s.sessionsBySlug,
            [slug]: list.filter((x) => x.id !== session),
          },
          selectedSession:
            s.selectedSession === session ? null : s.selectedSession,
        };
      }
      return s;
    }),
}));
