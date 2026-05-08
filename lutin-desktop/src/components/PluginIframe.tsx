import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  setActiveSession,
  ttsCancel,
  ttsCloseStream,
  ttsEnsureBackend,
  ttsOpenStream,
  ttsSpeak,
  workflowOpenPlugin,
  workflowSessionClose,
  workflowSessionOpen,
  workflowSessionRequest,
  workflowSessionSubscribe,
} from "../api";
import type {
  PluginOpened,
  SessionId,
  Slug,
  TtsBackend,
  TtsSpeed,
  TtsStreamId,
  WorkflowId,
} from "../types";
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
  const [iframeLoaded, setIframeLoaded] = useState(false);

  // Resolve plugin URL.
  useEffect(() => {
    let cancelled = false;
    setError(null);
    setOpened(null);
    setIframeLoaded(false);
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

  // Wire the MessagePort handshake + the bytes pump. Runs once *all*
  // three preconditions are true: bundle resolved, engine bridge open,
  // and iframe finished loading. We can't rely on attaching a fresh
  // `load` listener here — `opened` and `bridgeReady` may flip after
  // the iframe has already loaded, in which case the listener never
  // fires and the iframe stays blank. Tracking `iframeLoaded` as state
  // (set by an inline `onLoad` on the JSX) sidesteps the race.
  useEffect(() => {
    const iframe = iframeRef.current;
    if (!iframe || !opened || !bridgeReady || !iframeLoaded) return;

    let port: MessagePort | null = null;
    let unsubscribePromise: Promise<{ id: () => number }> | null = null;
    let cancelled = false;

    // Permission gate. The plugin's manifest names which capabilities
    // the chrome should forward; calls to undeclared capabilities are
    // rejected here, before any native side-effect runs. The bytes
    // pump (request/response/broadcast) is implicit — every plugin
    // gets it; capabilities are the gated surface.
    const permissions = new Set(opened.manifest.permissions);
    const allow = (perm: string) => permissions.has(perm);

    type RequestMsg = { kind: "request"; request_id: number; body: Uint8Array | number[] };
    type NotificationMsg = { kind: "notification"; body: string; title?: string };
    type TtsCallMsg = {
      kind: "tts-call";
      request_id: number;
      method: "ensureBackend" | "openStream" | "speak" | "cancel" | "closeStream";
      // Method-shaped args; we type-narrow below.
      args: Record<string, unknown>;
    };
    type IframeMsg = RequestMsg | NotificationMsg | TtsCallMsg;

    // Tracks streams opened by *this* iframe so cancel/close/speak
    // calls can't reach across to a stream id leaked from another
    // workflow. Pure defense-in-depth: every TTS command is already
    // capability-gated, but capability + ownership is the right shape.
    const ownedStreams = new Set<TtsStreamId>();

    const handleRequest = async (port: MessagePort, msg: RequestMsg) => {
      const bytes = msg.body instanceof Uint8Array ? msg.body : Uint8Array.from(msg.body);
      try {
        const reply = await workflowSessionRequest(session, bytes);
        port.postMessage({ kind: "response", request_id: msg.request_id, body: reply });
      } catch (err) {
        // Surface to the iframe so the plugin can show the failure;
        // an `error` field on the response keeps request_id
        // correlation intact.
        port.postMessage({
          kind: "response",
          request_id: msg.request_id,
          error: String(err),
        });
      }
    };

    const hasTts = (opened.manifest.capabilities ?? []).includes("tts");

    const handleTtsCall = async (port: MessagePort, msg: TtsCallMsg) => {
      const reply = (body?: unknown, error?: string) => {
        const env: Record<string, unknown> = { kind: "response", request_id: msg.request_id };
        if (error !== undefined) env.error = error;
        else env.body = body;
        port.postMessage(env);
      };
      if (!hasTts) {
        // Mirror the Rust capability message shape so workflows can
        // detect the failure mode without parsing strings — but a
        // plain message is fine for v1, this should never fire from
        // a well-formed plugin since the shim doesn't expose
        // `lutin.tts` without the capability.
        console.warn(`[plugin ${workflow}] denied: tts not in manifest.capabilities`);
        reply(undefined, "tts capability not declared");
        return;
      }
      // Pull a stream id out of `args`, defending against a bad-shape
      // envelope. Without this, a non-number `streamId` would slip
      // past `ownedStreams.has` (always `false`) and surface as
      // "stream not owned by this workflow" — misleading. Tauri serde
      // is the authoritative parse for the rest of the args; this is
      // the one field we reuse locally so it has to be checked here.
      const ownedStreamId = (args: Record<string, unknown>): TtsStreamId | string => {
        const id = args.streamId;
        if (typeof id !== "number" || !Number.isFinite(id)) {
          return "streamId missing or not a number";
        }
        if (!ownedStreams.has(id)) return "stream not owned by this workflow";
        return id;
      };
      try {
        switch (msg.method) {
          case "ensureBackend": {
            const { backend } = msg.args as { backend: TtsBackend };
            await ttsEnsureBackend(backend);
            reply(null);
            return;
          }
          case "openStream": {
            const { backend } = msg.args as { backend: TtsBackend };
            const id = await ttsOpenStream(backend, session);
            ownedStreams.add(id);
            reply(id);
            return;
          }
          case "speak": {
            const id = ownedStreamId(msg.args);
            if (typeof id !== "number") return reply(undefined, id);
            const { text, speed } = msg.args as { text: string; speed: TtsSpeed };
            await ttsSpeak(id, text, speed);
            reply(null);
            return;
          }
          case "cancel": {
            const id = ownedStreamId(msg.args);
            if (typeof id !== "number") return reply(undefined, id);
            await ttsCancel(id);
            reply(null);
            return;
          }
          case "closeStream": {
            const id = ownedStreamId(msg.args);
            if (typeof id !== "number") return reply(undefined, id);
            await ttsCloseStream(id);
            ownedStreams.delete(id);
            reply(null);
            return;
          }
          default: {
            // Unknown method — without this branch the iframe's
            // promise would hang forever (the shim's pending map
            // never resolves). The TS union narrows `msg.method` to
            // `never` here, so cast to surface the bad value.
            const m = (msg as { method: string }).method;
            reply(undefined, `unknown tts method: ${m}`);
            return;
          }
        }
      } catch (err) {
        reply(undefined, String(err));
      }
    };

    const handleNotification = (msg: NotificationMsg) => {
      if (!allow("notification")) {
        console.warn(
          `[plugin ${workflow}] denied: notification not in manifest.permissions`,
        );
        return;
      }
      console.info("[plugin notification]", msg.title ?? "Plugin", msg.body);
      try {
        if ("Notification" in window && Notification.permission === "granted") {
          new Notification(msg.title ?? "Plugin", { body: msg.body });
        }
      } catch { /* non-fatal */ }
    };

    const channel = new MessageChannel();
    port = channel.port1;
    port.onmessage = (e) => {
      const msg = e.data as IframeMsg | undefined;
      if (!msg || !port) return;
      if (msg.kind === "request") void handleRequest(port, msg);
      else if (msg.kind === "tts-call") void handleTtsCall(port, msg);
      else if (msg.kind === "notification") handleNotification(msg);
    };
    port.start();

    unsubscribePromise = (async () => {
      const ch = await workflowSessionSubscribe(session, (body) => {
        port?.postMessage({ kind: "broadcast", body });
      });
      return { id: () => ch.id };
    })();

    // Per-session transcription deliveries. Rust gates emission on
    // the manifest declaring `receive_transcription`, but we keep the
    // listener attached unconditionally — if Rust says deliver, deliver.
    // The shim only exposes `onTranscription` to the plugin when the
    // manifest opted in, so a workflow that didn't declare it can't
    // observe these even if some other path delivered one.
    const transcriptionTopic = `transcription:${session}`;
    const transcriptionUnlisten = listen<{ text: string; source: string }>(
      transcriptionTopic,
      (e) => {
        port?.postMessage({
          kind: "transcription",
          text: e.payload.text,
          source: e.payload.source,
        });
      },
    );

    // Tell Rust which session is active so dispatch can resolve
    // `Target::ActiveWorkflow`. Capabilities ride along so the routing
    // gate doesn't need a second lookup.
    setActiveSession({
      session,
      workflow,
      capabilities: opened.manifest.capabilities ?? [],
    }).catch(() => { /* non-fatal — gate falls back to clipboard */ });

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

    return () => {
      cancelled = true;
      // Tear down any TTS streams the workflow opened but didn't
      // close. Without this, the CP-side stream registry leaks slots
      // (and a queued utterance could keep playing after the iframe
      // is gone). Close is best-effort — transport drops are fine.
      for (const id of ownedStreams) {
        ttsCloseStream(id).catch(() => {});
      }
      ownedStreams.clear();
      port?.close();
      transcriptionUnlisten.then((u) => u()).catch(() => {});
      // Best-effort clear; if another iframe mounts immediately it
      // will overwrite this with its own `setActiveSession` call.
      setActiveSession(null).catch(() => {});
      // Tauri Channels can't be unregistered explicitly; the bridge
      // drops dead subscribers on the next broadcast send. Holding
      // the promise prevents an unused-var warning while documenting
      // the intent.
      void unsubscribePromise; void cancelled;
    };
  }, [opened, bridgeReady, iframeLoaded, slug, session, workflow]);

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

  // Staged status: bundle → engine → iframe boot. The iframe always
  // mounts (so its load fires deterministically) but we overlay an
  // indicator until everything's wired and the plugin can take over.
  const fullyReady = opened !== null && bridgeReady && iframeLoaded;
  const stage: { label: string; sub: string } | null = !opened
    ? { label: "Fetching plugin bundle…", sub: workflow }
    : !bridgeReady
      ? { label: "Connecting to engine…", sub: shortSession(session) }
      : !iframeLoaded
        ? { label: "Starting plugin…", sub: opened.manifest.display_name || workflow }
        : null;

  return (
    <div style={{ position: "relative", width: "100%", height: "100%" }}>
      {opened && (
        <iframe
          ref={iframeRef}
          src={opened.url}
          // `allow-same-origin` keeps the iframe on its
          // `lutin-plugin://<id>` origin instead of letting the sandbox
          // attribute force it to `null`. The cross-origin isolation we
          // rely on comes from the custom scheme (chrome lives on a
          // different scheme/host), not from the sandbox — origin
          // `null` actually defeats it because `null` ≠ `null` in
          // postMessage targeting and CORS checks refuse
          // `Access-Control-Allow-Origin: *` for null origins.
          sandbox="allow-scripts allow-same-origin"
          title={opened.manifest.display_name || workflow}
          onLoad={() => setIframeLoaded(true)}
          style={{
            width: "100%",
            height: "100%",
            border: 0,
            visibility: fullyReady ? "visible" : "hidden",
          }}
        />
      )}
      {stage && (
        <div
          className={styles.placeholder}
          style={{
            position: "absolute",
            inset: 0,
            display: "flex",
            justifyContent: "center",
            alignItems: "center",
            background: "var(--bg-0)",
          }}
        >
          <div style={{ display: "flex", flexDirection: "column", gap: "var(--s-2)", alignItems: "center" }}>
            <Spinner />
            <div className={styles.placeholderTitle}>{stage.label}</div>
            <div className={styles.placeholderSub}><code>{stage.sub}</code></div>
          </div>
        </div>
      )}
    </div>
  );
}

function Spinner() {
  return (
    <svg width="28" height="28" viewBox="0 0 50 50" aria-hidden>
      <circle
        cx="25" cy="25" r="20" fill="none"
        stroke="currentColor" strokeWidth="4" strokeLinecap="round"
        strokeDasharray="80 60"
        opacity="0.6"
      >
        <animateTransform
          attributeName="transform" type="rotate"
          from="0 25 25" to="360 25 25" dur="0.9s" repeatCount="indefinite"
        />
      </circle>
    </svg>
  );
}

function shortSession(id: string): string {
  return id.length > 12 ? id.slice(0, 10) + "…" : id;
}

function originOf(url: string): string {
  try {
    return new URL(url).origin;
  } catch {
    return "*";
  }
}
