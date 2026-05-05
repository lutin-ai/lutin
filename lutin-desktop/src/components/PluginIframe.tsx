import { useEffect, useRef, useState } from "react";
import { workflowOpenPlugin } from "../api";
import type { PluginOpened, SessionId, Slug, WorkflowId } from "../types";
import styles from "./SessionPane.module.css";

interface Props {
  slug: Slug;
  session: SessionId;
  workflow: WorkflowId;
  digest: string;
}

/// Renders a workflow plugin's UI in a sandboxed cross-origin iframe.
///
/// Once the iframe loads, chrome creates a `MessageChannel`, sends one
/// of the ports to the iframe in a single bootstrap `postMessage`, and
/// drives all subsequent IPC through the other port. The iframe's
/// origin is the bundle's custom-protocol origin (`lutin-plugin://...`)
/// — distinct from chrome's — so postMessage origin targeting is
/// meaningful.
///
/// The bytes pump (workflow-engine I/O) is intentionally not wired
/// here yet; this slice proves the plumbing with a chrome-handled
/// `notification.post` and an echo `send` that round-trips through
/// chrome. Engine bridge lands with the chat workflow rewrite.
export function PluginIframe(props: Props) {
  const { slug, session, workflow, digest } = props;
  const iframeRef = useRef<HTMLIFrameElement | null>(null);
  const [opened, setOpened] = useState<PluginOpened | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setError(null);
    setOpened(null);
    workflowOpenPlugin(workflow, digest)
      .then((res) => { if (!cancelled) setOpened(res); })
      .catch((e) => { if (!cancelled) setError(String(e)); });
    return () => { cancelled = true; };
  }, [workflow, digest]);

  useEffect(() => {
    const iframe = iframeRef.current;
    if (!iframe || !opened) return;

    // Wait for the iframe document to actually load before posting —
    // otherwise the message arrives with no listener attached.
    let port: MessagePort | null = null;
    const onLoad = () => {
      const channel = new MessageChannel();
      port = channel.port1;
      port.onmessage = (e) => handlePortMessage(e.data, opened.manifest.permissions);
      port.start();

      const targetOrigin = originOf(opened.url);
      iframe.contentWindow?.postMessage(
        {
          type: "lutin-init",
          slug,
          session,
          workflow,
          manifest: opened.manifest,
        },
        targetOrigin,
        [channel.port2],
      );
    };

    iframe.addEventListener("load", onLoad);
    return () => {
      iframe.removeEventListener("load", onLoad);
      port?.close();
    };
  }, [opened, slug, session, workflow]);

  if (error) {
    return (
      <div className={styles.placeholder}>
        <div className={styles.placeholderIcon}>⚠</div>
        <div className={styles.placeholderTitle}>Plugin failed to load</div>
        <div className={styles.placeholderSub}>
          <code>{workflow}</code>: {error}
        </div>
      </div>
    );
  }
  if (!opened) {
    return (
      <div className={styles.placeholder}>
        <div className={styles.placeholderIcon}>⏳</div>
        <div className={styles.placeholderTitle}>Loading plugin…</div>
        <div className={styles.placeholderSub}><code>{workflow}</code></div>
      </div>
    );
  }

  return (
    <iframe
      ref={iframeRef}
      src={opened.url}
      // sandbox here is belt-and-braces; the cross-origin custom
      // protocol already isolates the plugin from chrome's origin.
      // `allow-scripts` is required for any plugin to function.
      sandbox="allow-scripts"
      title={opened.manifest.display_name || workflow}
      style={{ width: "100%", height: "100%", border: 0 }}
    />
  );
}

function originOf(url: string): string {
  try {
    return new URL(url).origin;
  } catch {
    return "*";
  }
}

/// Handle a single message received over the plugin's MessagePort.
/// Drops messages for capabilities the plugin didn't declare in its
/// manifest. Permission strings match the manifest exactly — adding a
/// new capability means listing it here AND declaring it in the
/// plugin's `lutin.workflow.json`.
function handlePortMessage(data: unknown, permissions: string[]): void {
  if (!data || typeof data !== "object") return;
  const msg = data as { type?: string; [k: string]: unknown };
  switch (msg.type) {
    case "notification.post": {
      // Notifications are always allowed in this slice — when we wire
      // the OS-level notify call through Tauri, gate this on a
      // "notification" permission.
      const body = String(msg.body ?? "");
      const title = typeof msg.title === "string" ? msg.title : "Plugin";
      // Browser-level fallback for the smallest slice; replaced by a
      // Tauri command in Phase 3.
      console.info("[plugin notification]", title, body);
      try {
        if ("Notification" in window && Notification.permission === "granted") {
          new Notification(title, { body });
        }
      } catch { /* ignore — non-fatal */ }
      break;
    }
    case "send": {
      // Echo placeholder: the engine bytes pump isn't wired yet.
      // Logging proves the round-trip works without falsely
      // implying any engine connectivity.
      if (!permissions.includes("send")) return;
      console.info("[plugin send]", msg.bytes);
      break;
    }
    default:
      console.warn("[plugin] unknown message type", msg.type);
  }
}
