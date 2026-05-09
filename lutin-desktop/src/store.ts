import { create } from "zustand";
import type {
  ConnState,
  CpEvent,
  ProjectInfo,
  SessionInfo,
  Slug,
  SessionId,
  SubAgentRow,
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
  /// Sub-agent surface state for sessions whose plugin declares
  /// the `sub_agents` capability (chat today). Keyed by session id;
  /// populated while the iframe is mounted, cleared on unmount.
  /// `selected` is the focused sub-agent id, or `null` for the
  /// parent chat. Selection is UI-only — never persisted to the
  /// engine — so the two fields share a lifetime and live together
  /// to make the "selected id refers to a known agent" invariant
  /// readable at the call site.
  chatStateBySession: Record<SessionId, { agents: SubAgentRow[]; selected: string | null }>;

  setConn: (c: ConnState) => void;
  setView: (v: AppView) => void;
  setSettings: (s: DesktopSettings) => void;
  setProjects: (p: ProjectInfo[]) => void;
  selectProject: (s: Slug | null) => void;
  setSessions: (slug: Slug, sessions: SessionInfo[]) => void;
  selectSession: (id: SessionId | null) => void;
  setSubAgents: (session: SessionId, agents: SubAgentRow[]) => void;
  selectSubAgent: (session: SessionId, id: string | null) => void;
  /// Drop a session's sub-agent state. Called by `PluginIframe` on
  /// unmount so a stale tree doesn't bleed into the next session.
  clearSubAgentState: (session: SessionId) => void;
  /// Merge live counters (from the iframe's `publishSummary`) into a
  /// session's `summary` so the sidebar's `ctx` column refreshes
  /// without a `ListSessions` round-trip. Persona / title / other
  /// workflow-written summary keys are preserved — only the token
  /// counters are touched here.
  setSessionSummary: (
    slug: Slug,
    session: SessionId,
    patch: {
      contextTokens: number | null;
      totalPromptTokens: number;
      totalCompletionTokens: number;
      /** `undefined` = leave the existing persona/title untouched
       *  (workflow didn't include the field in this tick). `null` =
       *  explicit "no persona / no title yet". */
      persona?: string | null;
      title?: string | null;
    },
  ) => void;
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
  chatStateBySession: {},

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
  setSubAgents: (session, agents) =>
    set((s) => {
      const prev = s.chatStateBySession[session];
      // Validate the live selection against the new agent list — if
      // the focused agent is gone (cancelled, parent reaped it), drop
      // it back to the parent chat. Without this the sidebar could
      // keep highlighting an id that no longer renders, and clicking
      // "back" would have no effect.
      const selected =
        prev?.selected != null && agents.some((a) => a.id === prev.selected)
          ? prev.selected
          : null;
      return {
        chatStateBySession: {
          ...s.chatStateBySession,
          [session]: { agents, selected },
        },
      };
    }),
  selectSubAgent: (session, id) =>
    set((s) => {
      const prev = s.chatStateBySession[session] ?? { agents: [], selected: null };
      return {
        chatStateBySession: {
          ...s.chatStateBySession,
          [session]: { ...prev, selected: id },
        },
      };
    }),
  clearSubAgentState: (session) =>
    set((s) => {
      const { [session]: _drop, ...rest } = s.chatStateBySession;
      return { chatStateBySession: rest };
    }),
  setSessionSummary: (slug, session, patch) =>
    set((s) => {
      const list = s.sessionsBySlug[slug];
      if (!list) return s;
      const idx = list.findIndex((x) => x.id === session);
      if (idx === -1) return s;
      const prev = list[idx];
      const nextSummary = {
        ...(prev.summary ?? {}),
        context_tokens: patch.contextTokens,
        total_prompt_tokens: patch.totalPromptTokens,
        total_completion_tokens: patch.totalCompletionTokens,
        ...(patch.persona !== undefined ? { persona: patch.persona } : {}),
        ...(patch.title !== undefined ? { title: patch.title } : {}),
      };
      const nextList = list.slice();
      nextList[idx] = { ...prev, summary: nextSummary };
      return { sessionsBySlug: { ...s.sessionsBySlug, [slug]: nextList } };
    }),

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
        const { [session]: _drop, ...restChatState } = s.chatStateBySession;
        return {
          sessionsBySlug: {
            ...s.sessionsBySlug,
            [slug]: list.filter((x) => x.id !== session),
          },
          selectedSession:
            s.selectedSession === session ? null : s.selectedSession,
          chatStateBySession: restChatState,
        };
      }
      return s;
    }),
}));
