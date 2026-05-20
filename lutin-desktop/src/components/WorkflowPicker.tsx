import { useEffect, useState } from "react";
import { cpSendOk } from "../api";
import { useApp } from "../store";
import type { WorkflowInfo } from "../types";
import { Picker } from "./Picker";
import modalStyles from "./Modal.module.css";

export interface WorkflowPickerProps {
  onClose: () => void;
}

interface WfItem {
  id: string;
  label: string;
  sub?: string;
  raw: WorkflowInfo;
}

export function WorkflowPicker({ onClose }: WorkflowPickerProps) {
  const selectedProject = useApp((s) => s.selectedProject);
  const applyEvent = useApp((s) => s.applyEvent);
  const selectSession = useApp((s) => s.selectSession);
  const view = useApp((s) => s.view);
  const setView = useApp((s) => s.setView);

  const [workflows, setWorkflows] = useState<WorkflowInfo[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    cpSendOk("ListWorkflows")
      .then((r) => {
        if (cancelled) return;
        if (typeof r === "object" && "Workflows" in r) {
          setWorkflows([...r.Workflows].sort((a, b) => a.display_name.localeCompare(b.display_name)));
        }
      })
      .catch((e) => { if (!cancelled) setError(String(e)); });
    return () => { cancelled = true; };
  }, []);

  const start = async (workflowId: string) => {
    if (!selectedProject) return;
    try {
      const r = await cpSendOk({
        StartSession: { slug: selectedProject, workflow: workflowId },
      });
      if (typeof r === "object" && "SessionStarted" in r) {
        const info = r.SessionStarted.info;
        applyEvent({ SessionStarted: { slug: selectedProject, info } });
        selectSession(info.id);
        if (view.kind === "settings") setView({ kind: "project" });
      }
      onClose();
    } catch (e) {
      setError(String(e));
    }
  };

  const items: WfItem[] = workflows.map((w) => ({
    id: w.id,
    label: w.display_name,
    sub: w.id,
    raw: w,
  }));

  return (
    <Picker
      title={selectedProject ? "New session" : "Select a project first"}
      placeholder="Workflow name…"
      items={selectedProject ? items : []}
      onClose={onClose}
      onSelect={(item) => start(item.id)}
      renderPreview={(item) => {
        if (!item) {
          if (error) return <div className={modalStyles.previewEmpty}>{error}</div>;
          if (!selectedProject)
            return <div className={modalStyles.previewEmpty}>Open the project picker (space p) first.</div>;
          return null;
        }
        return (
          <>
            <div className={modalStyles.previewTitle}>{item.raw.display_name}</div>
            <div className={modalStyles.previewSub}>{item.raw.id}</div>
            <div className={modalStyles.previewGrid}>
              <span className={modalStyles.previewKey}>Workflow id</span>
              <span className={modalStyles.previewVal}>{item.raw.id}</span>
              <span className={modalStyles.previewKey}>Digest</span>
              <span className={modalStyles.previewVal} style={{ fontFamily: "var(--font-mono)", fontSize: "var(--fs-sm)" }}>
                {item.raw.digest.slice(0, 16)}…
              </span>
            </div>
            <div className={modalStyles.previewEmpty}>
              Press enter to start a new session in <code>{selectedProject}</code>.
            </div>
          </>
        );
      }}
    />
  );
}
