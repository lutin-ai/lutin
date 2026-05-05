import { useEffect, useRef, useState } from "react";
import {
  workflowOpenPlugin,
  workflowSessionClose,
  workflowSessionOpen,
  workflowSessionRequest,
  workflowSessionSubscribe,
} from "../api";
import type { PluginOpened, SessionId, Slug, WorkflowId } from "../types";
import styles from "./SessionPane.module.css";

interface Props {
  slug: Slug;
  session: SessionId;
  workflow: WorkflowId;
  digest: string;
}

/// Renders a workflow plugin's UI in a sandboxed cross-origin iframe
/// and proxies the bytes pump between iframe and engine.
///
/// Lifecycle:
///   1. Resolve the iframe URL via `workflow_open_plugin` (fetches +
///      caches the bundle on miss).
///   2. Open the engine WebSocket via `workflow_session_open`. Token
///      never crosses to JS; chrome holds it.
///   3. Subscribe to engine broadcasts; on each broadcast body, post
///      `{ kind: "broadcast", body }` to the iframe over the port.
///   4. On iframe `lutin-init` load, transfer one MessagePort.
///   5. Forward iframe `request` messages → `workflow_session_request`,
///      reply with `{ kind: "response", request_id, body }`.
///   6. Forward iframe `notification` messages → chrome notification.
///   7. On unmount: close the engine bridge.
export function PluginIframe(props: Props) {
  const { slug, session, workflow, digest } = props;
  const iframeRef = useRef<HTMLIFrameElement | null>(null);
  const [opened, setOpened] = useState<PluginOpened | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [bridgeReady, setBridgeReady] = useState(false);

  // Resolve plugin URL.
  useEffect(() => {
    let cancelled = false;
    setError(null);
    setOpened(null);
    workflowOpenPlugin(workflow, digest)
      .then((res) => { if (!cancelled) setOpened(res); })
      .catch((e) => { if (!cancelled) setError(String(e)); });
    return () => { cancelled = true; };
  }, [workflow, digest]);

  // Open the engine bridge for this session. Tear down on unmount.
  useEffect(() => {
    let cancelled = false;
    setBridgeReady(false);
    workflowSessionOpen(slug, session)
      .then(() => { if (!cancelled) setBridgeReady(true); })
      .catch((e) => { if (!cancelled) setError((prev) => prev ?? String(e)); });
    return () => {
      cancelled = true;
      // Best-effort; the bridge map is keyed by session id and is
      // safe to close even if open hadn't completed yet.
      workflowSessionClose(session).catch(() => {});
    };
  }, [slug, session]);

  // Wire the MessagePort handshake + the bytes pump.
  useEffect(() => {
    const iframe = iframeRef.current;
    if (!iframe || !opened || !bridgeReady) return;

    let port: MessagePort | null = null;
    let unsubscribePromise: Promise<{ id: () => number }> | null = null;
    let cancelled = false;

    const onLoad = () => {
      const channel = new MessageChannel();
      port = channel.port1;
      port.onmessage = async (e) => {
        const msg = e.data as
          | { kind: "request"; request_id: number; body: Uint8Array | number[] }
          | { kind: "notification"; body: string; title?: string }
          | undefined;
        if (!msg || !port) return;
        if (msg.kind === "request") {
          const bytes = msg.body instanceof Uint8Array
            ? msg.body
            : Uint8Array.from(msg.body);
          try {
            const reply = await workflowSessionRequest(session, bytes);
            port.postMessage({
              kind: "response",
              request_id: msg.request_id,
              body: reply,
            });
          } catch (err) {
            // Surface to the iframe so the plugin can show the
            // failure; using an `error` field on the response keeps
            // the request_id correlation intact.
            port.postMessage({
              kind: "response",
              request_id: msg.request_id,
              error: String(err),
            });
          }
          return;
        }
        if (msg.kind === "notification") {
          console.info("[plugin notification]", msg.title ?? "Plugin", msg.body);
          try {
            if ("Notification" in window && Notification.permission === "granted") {
              new Notification(msg.title ?? "Plugin", { body: msg.body });
            }
          } catch { /* non-fatal */ }
        }
      };
      port.start();

      // Subscribe to engine broadcasts → forward to iframe. The
      // promise resolves before the channel is wired, but the bridge
      // pump won't fire any broadcasts before subscribe lands as long
      // as the iframe hasn't asked for any yet (engines push events
      // only in response to subscribe-style requests).
      unsubscribePromise = (async () => {
        const ch = await workflowSessionSubscribe(session, (body) => {
          port?.postMessage({ kind: "broadcast", body });
        });
        return { id: () => ch.id };
      })();

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
      cancelled = true;
      iframe.removeEventListener("load", onLoad);
      port?.close();
      // Tauri Channels can't be unregistered explicitly; the bridge
      // drops dead subscribers on the next broadcast send. Holding
      // the promise prevents an unused-var warning while documenting
      // the intent.
      void unsubscribePromise; void cancelled;
    };
  }, [opened, bridgeReady, slug, session, workflow]);

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
