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
  // De-dupe by slug at write time. ListProjects on connect and the
  // ProjectCreated event race each other, and the event has already
  // been applied by the time the list reply lands; without this guard
  // a project ends up in `projects` twice → React duplicate-key
  // warning → sidebar click handlers bound to the wrong row.
  setProjects: (projects) => {
    const seen = new Set<string>();
    const dedup: ProjectInfo[] = [];
    for (const p of projects) {
      if (seen.has(p.slug)) continue;
      seen.add(p.slug);
      dedup.push(p);
    }
    set({ projects: dedup });
  },
  selectProject: (slug) => set({ selectedProject: slug, selectedSession: null }),
  setSessions: (slug, sessions) => {
    const seen = new Set<string>();
    const dedup: SessionInfo[] = [];
    for (const sess of sessions) {
      if (seen.has(sess.id)) continue;
      seen.add(sess.id);
      dedup.push(sess);
    }
    set((s) => ({ sessionsBySlug: { ...s.sessionsBySlug, [slug]: dedup } }));
  },
  selectSession: (id) => set({ selectedSession: id }),

  applyEvent: (event) =>
    set((s) => {
      if ("ProjectCreated" in event) {
        const exists = s.projects.some((p) => p.slug === event.ProjectCreated.slug);
        if (exists) return s;
        return { projects: [...s.projects, event.ProjectCreated] };
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
