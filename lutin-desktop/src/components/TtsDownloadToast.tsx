// Small fixed-position pill that surfaces TTS-backend download
// progress. Hooks into the `cp:event` Tauri event stream so it's
// independent of which workflow triggered the `EnsureTtsBackend` —
// only one download runs at a time CP-side, so a single global
// indicator is the right shape.
//
// Hides itself a beat after `downloaded === total` so the user gets
// a glimpse of "100%" before it disappears, and also auto-hides if
// no progress event arrives for 30 s (CP probably crashed mid-fetch).

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type { CpEvent } from "../types";

interface Progress {
  file: string;
  downloaded: number;
  total: number | null;
}

const STALE_AFTER_MS = 30_000;
const DONE_LINGER_MS = 1500;

export function TtsDownloadToast() {
  const [progress, setProgress] = useState<Progress | null>(null);

  useEffect(() => {
    let staleTimer: ReturnType<typeof setTimeout> | null = null;
    let doneTimer: ReturnType<typeof setTimeout> | null = null;

    const armStale = () => {
      if (staleTimer) clearTimeout(staleTimer);
      staleTimer = setTimeout(() => setProgress(null), STALE_AFTER_MS);
    };

    const unlisten = listen<CpEvent>("cp:event", (e) => {
      const ev = e.payload;
      if (!("TtsBackendDownload" in ev)) return;
      const { file, downloaded, total } = ev.TtsBackendDownload;
      if (doneTimer) clearTimeout(doneTimer);
      setProgress({ file, downloaded, total });
      armStale();
      if (total !== null && downloaded >= total) {
        doneTimer = setTimeout(() => setProgress(null), DONE_LINGER_MS);
      }
    });

    return () => {
      unlisten.then((u) => u()).catch(() => {});
      if (staleTimer) clearTimeout(staleTimer);
      if (doneTimer) clearTimeout(doneTimer);
    };
  }, []);

  if (!progress) return null;
  const { file, downloaded, total } = progress;
  const pct = total !== null && total > 0
    ? Math.min(100, Math.floor((downloaded / total) * 100))
    : null;
  return (
    <div
      style={{
        position: "fixed",
        bottom: "16px",
        right: "16px",
        padding: "10px 14px",
        background: "var(--bg-1, #1c1c1e)",
        color: "var(--fg-0, #fff)",
        border: "1px solid var(--border, rgba(255,255,255,0.12))",
        borderRadius: "10px",
        boxShadow: "0 4px 16px rgba(0,0,0,0.3)",
        fontSize: "12px",
        display: "flex",
        flexDirection: "column",
        gap: "6px",
        minWidth: "240px",
        zIndex: 9999,
      }}
    >
      <div style={{ display: "flex", justifyContent: "space-between", gap: "12px" }}>
        <span style={{ opacity: 0.85 }}>Downloading {file}</span>
        <span style={{ opacity: 0.7, fontVariantNumeric: "tabular-nums" }}>
          {pct !== null ? `${pct}%` : formatBytes(downloaded)}
        </span>
      </div>
      <div
        style={{
          height: "4px",
          background: "rgba(255,255,255,0.1)",
          borderRadius: "2px",
          overflow: "hidden",
        }}
      >
        <div
          style={{
            height: "100%",
            width: pct !== null ? `${pct}%` : "30%",
            background: "currentColor",
            transition: "width 0.4s ease-out",
            // Indeterminate hint when total is unknown — solid bar
            // looks wrong, faint bar reads as "still working".
            opacity: pct !== null ? 0.9 : 0.5,
          }}
        />
      </div>
      <div style={{ opacity: 0.55, fontVariantNumeric: "tabular-nums" }}>
        {formatBytes(downloaded)}
        {total !== null && ` / ${formatBytes(total)}`}
      </div>
    </div>
  );
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}
