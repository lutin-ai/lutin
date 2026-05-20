import { latestActivity, relativeTime } from "../relativeTime";
import { useApp } from "../store";
import type { SessionInfo } from "../types";
import modalStyles from "./Modal.module.css";
import { Picker } from "./Picker";

export interface ProjectPickerProps {
  onClose: () => void;
}

interface ProjectItem {
  id: string;
  label: string;
  sub: string;
  current: boolean;
}

export function ProjectPicker({ onClose }: ProjectPickerProps) {
  const projects = useApp((s) => s.projects);
  const sessionsBySlug = useApp((s) => s.sessionsBySlug);
  const selected = useApp((s) => s.selectedProject);
  const select = useApp((s) => s.selectProject);
  const view = useApp((s) => s.view);
  const setView = useApp((s) => s.setView);

  const items: ProjectItem[] = projects
    .map((p) => ({
      id: p.slug,
      label: p.display_name,
      sub: latestActivity(sessionsBySlug[p.slug] ?? []) ?? "",
      current: p.slug === selected,
    }))
    .sort((a, b) => {
      if (a.current && !b.current) return -1;
      if (!a.current && b.current) return 1;
      return (b.sub ?? "").localeCompare(a.sub ?? "");
    });

  return (
    <Picker
      title="Switch project"
      placeholder="Project name…"
      items={items}
      onClose={onClose}
      onSelect={(item) => {
        if (view.kind === "settings") setView({ kind: "project" });
        select(item.id);
        onClose();
      }}
      renderSub={(item) => (
        <>
          {item.current && <span className={modalStyles.stateDot} data-state="running" aria-hidden />}
          <span>{relativeTime(item.sub) || "—"}</span>
        </>
      )}
      renderPreview={(item) => {
        if (!item) return null;
        const sessions = sessionsBySlug[item.id] ?? [];
        return <ProjectPreview slug={item.id} name={item.label} sessions={sessions} />;
      }}
    />
  );
}

function ProjectPreview({ slug, name, sessions }: { slug: string; name: string; sessions: SessionInfo[] }) {
  // Sessions grouped by workflow id, sorted most-recent-first within
  // each group. Mirrors the sidebar's grouping logic — same data,
  // tighter layout.
  const groups: Record<string, SessionInfo[]> = {};
  for (const s of sessions) (groups[s.workflow] ??= []).push(s);
  for (const list of Object.values(groups)) {
    list.sort((a, b) => {
      const ka = a.summary?.last_activity ?? a.created_at;
      const kb = b.summary?.last_activity ?? b.created_at;
      return kb.localeCompare(ka);
    });
  }
  const workflowIds = Object.keys(groups).sort();

  return (
    <>
      <div className={modalStyles.previewTitle}>{name}</div>
      <div className={modalStyles.previewSub}>{slug}</div>
      {workflowIds.length === 0 ? (
        <div className={modalStyles.previewEmpty}>No sessions yet.</div>
      ) : (
        workflowIds.map((wf) => (
          <div key={wf} className={modalStyles.previewGroup}>
            <div className={modalStyles.previewGroupHead}>{wf}</div>
            {groups[wf].map((s) => {
              const title = s.summary?.title?.trim() || s.id.slice(0, 8);
              const time = relativeTime(s.summary?.last_activity ?? s.created_at);
              const state = s.state === "Running" ? "running" : "dormant";
              return (
                <div key={s.id} className={modalStyles.previewRow}>
                  <span className={modalStyles.stateDot} data-state={state} aria-hidden />
                  <span className={modalStyles.previewRowTitle}>{title}</span>
                  <span className={modalStyles.previewRowMeta}>{time}</span>
                </div>
              );
            })}
          </div>
        ))
      )}
    </>
  );
}
