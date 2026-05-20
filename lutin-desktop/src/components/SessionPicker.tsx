import { relativeTime } from "../relativeTime";
import { useApp } from "../store";
import type { SessionInfo } from "../types";
import modalStyles from "./Modal.module.css";
import { Picker } from "./Picker";

export interface SessionPickerProps {
  onClose: () => void;
}

interface SessionItem {
  id: string;
  label: string;
  sub: string;
  raw: SessionInfo;
}

export function SessionPicker({ onClose }: SessionPickerProps) {
  const selectedProject = useApp((s) => s.selectedProject);
  const sessionsBySlug = useApp((s) => s.sessionsBySlug);
  const selectSession = useApp((s) => s.selectSession);
  const view = useApp((s) => s.view);
  const setView = useApp((s) => s.setView);

  const sessions = selectedProject ? sessionsBySlug[selectedProject] ?? [] : [];

  const items: SessionItem[] = sessions
    .map((s) => {
      const title = s.summary?.title?.trim() || s.id.slice(0, 8);
      return {
        id: s.id,
        label: `${s.workflow}/${title}`,
        sub: s.summary?.last_activity ?? s.created_at,
        raw: s,
      };
    })
    .sort((a, b) => (b.sub ?? "").localeCompare(a.sub ?? ""));

  return (
    <Picker
      title={selectedProject ? "Switch session" : "No project selected"}
      placeholder="Session title or workflow…"
      items={items}
      onClose={onClose}
      onSelect={(item) => {
        if (view.kind === "settings") setView({ kind: "project" });
        selectSession(item.id);
        onClose();
      }}
      renderSub={(item) => {
        const state = item.raw.state === "Running" ? "running" : "dormant";
        return (
          <>
            <span className={modalStyles.stateDot} data-state={state} aria-hidden />
            <span>{relativeTime(item.sub) || "—"}</span>
          </>
        );
      }}
      renderPreview={(item) => (item ? <SessionPreview info={item.raw} /> : null)}
    />
  );
}

function formatTokens(n: number | null | undefined): string {
  if (n == null) return "—";
  if (n >= 1000) return `${(n / 1000).toFixed(n >= 10_000 ? 0 : 1)}k`;
  return String(n);
}

function SessionPreview({ info }: { info: SessionInfo }) {
  const title = info.summary?.title?.trim() || info.id.slice(0, 8);
  const state = info.state === "Running" ? "running" : "dormant";
  const lastActivity = info.summary?.last_activity ?? info.created_at;
  const persona = info.summary?.persona ?? "—";
  const model = info.summary?.model ?? "—";
  const messages = info.summary?.message_count ?? null;
  const ctx = info.summary?.context_tokens ?? null;
  const promptTokens = info.summary?.total_prompt_tokens ?? null;
  const completionTokens = info.summary?.total_completion_tokens ?? null;
  const preview = info.summary?.preview?.trim() ?? "";

  return (
    <>
      <div className={modalStyles.previewTitle}>{title}</div>
      <div className={modalStyles.previewSub}>
        <span className={modalStyles.stateDot} data-state={state} aria-hidden style={{ marginRight: 6, verticalAlign: "middle" }} />
        {info.workflow} · {state}
      </div>
      <div className={modalStyles.previewGrid}>
        <span className={modalStyles.previewKey}>Last active</span>
        <span className={modalStyles.previewVal}>{relativeTime(lastActivity) || "—"}</span>
        <span className={modalStyles.previewKey}>Persona</span>
        <span className={modalStyles.previewVal}>{persona}</span>
        <span className={modalStyles.previewKey}>Model</span>
        <span className={modalStyles.previewVal}>{model}</span>
        <span className={modalStyles.previewKey}>Messages</span>
        <span className={modalStyles.previewVal}>{messages ?? "—"}</span>
        <span className={modalStyles.previewKey}>Context</span>
        <span className={modalStyles.previewVal}>{formatTokens(ctx)}</span>
        <span className={modalStyles.previewKey}>Total in / out</span>
        <span className={modalStyles.previewVal}>
          {formatTokens(promptTokens)} / {formatTokens(completionTokens)}
        </span>
      </div>
      {preview && (
        <div className={modalStyles.previewExcerpt}>{preview}</div>
      )}
    </>
  );
}
