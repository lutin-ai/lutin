import { Channel, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  ActiveSession,
  ConnState,
  CpEvent,
  DesktopSettings,
  PluginOpened,
  Request,
  Response,
  ResponseOk,
  SessionId,
  Slug,
  WorkflowId,
} from "./types";

export async function cpSend(request: Request): Promise<Response> {
  return invoke("cp_send", { request });
}

/// Throws if the response is `Err` or unexpected shape; otherwise
/// returns the inner `ResponseOk`. Most call sites want the success
/// arm — this avoids duplicating the unwrap dance.
export async function cpSendOk(request: Request): Promise<ResponseOk> {
  const resp = await cpSend(request);
  if ("Ok" in resp) return resp.Ok;
  const errKey = Object.keys(resp.Err)[0];
  const errVal = (resp.Err as Record<string, unknown>)[errKey];
  throw new Error(`${errKey}: ${JSON.stringify(errVal)}`);
}

export async function cpStatus(): Promise<ConnState> {
  return invoke("cp_status");
}

export async function settingsGet(): Promise<DesktopSettings> {
  return invoke("settings_get");
}

export async function settingsSet(settings: DesktopSettings): Promise<void> {
  return invoke("settings_set", { new: settings });
}

/// Which backend is delivering global hotkey events. `plugin` =
/// X11/macOS/Windows (combos in our settings ARE the binding);
/// `portal` = Wayland (compositor owns the binding, our settings
/// strings are descriptive hints — the user re-binds in their
/// compositor config). The portal variant carries the id prefix and
/// snippet template so JS doesn't re-derive the format; if the wire
/// ever changes only the Rust `HYPRLAND_SNIPPET_TEMPLATE` const is
/// touched.
export type KeybindBackendInfo =
  | { kind: "plugin" }
  | {
      kind: "portal";
      id_prefix: string;
      snippet_template: string;
    };

export async function keybindBackend(): Promise<KeybindBackendInfo> {
  return invoke("keybind_backend");
}

/// Resolve a workflow's plugin bundle to an iframe URL + manifest.
/// Triggers a CP fetch on cache miss, so first-call latency depends on
/// the bundle size.
export async function workflowOpenPlugin(
  workflow: WorkflowId,
  digest: string,
): Promise<PluginOpened> {
  return invoke("workflow_open_plugin", { workflow, digest });
}

/// Open the engine WebSocket for a session. Token never crosses the
/// JS boundary; chrome holds it for the lifetime of the bridge.
export async function workflowSessionOpen(
  slug: Slug,
  session: SessionId,
): Promise<void> {
  return invoke("workflow_session_open", { slug, session });
}

/// Forward a request body to the engine. Resolves with the matching
/// `Frame::Payload` reply body.
export async function workflowSessionRequest(
  session: SessionId,
  body: Uint8Array,
): Promise<Uint8Array> {
  // Tauri serializes Uint8Array as a number array; the Rust side
  // expects `Vec<u8>`. The reply is also a number array, so we
  // Uint8Array.from() it on the way back.
  const reply: number[] = await invoke("workflow_session_request", {
    session,
    body: Array.from(body),
  });
  return Uint8Array.from(reply);
}

/// Subscribe to engine broadcasts for a session. Returns the active
/// Tauri channel — `chan.onmessage = cb` to receive bodies. Drop the
/// channel (let it GC) when done; chrome cleans up dead subscribers
/// when their next broadcast attempt fails.
export async function workflowSessionSubscribe(
  session: SessionId,
  onBody: (body: Uint8Array) => void,
): Promise<Channel<number[]>> {
  const channel = new Channel<number[]>();
  channel.onmessage = (msg) => onBody(Uint8Array.from(msg));
  await invoke("workflow_session_subscribe", { session, channel });
  return channel;
}

export async function workflowSessionClose(session: SessionId): Promise<void> {
  return invoke("workflow_session_close", { session });
}

/// Tell Rust which session iframe is currently in front. Drives
/// `Target::ActiveWorkflow` resolution and the capability gate for
/// per-session transcription delivery. `null` = no plugin iframe
/// mounted (e.g. Settings tab).
export async function setActiveSession(active: ActiveSession | null): Promise<void> {
  return invoke("set_active_session", { active });
}

type EventHandlers = {
  onConnected?: () => void;
  onDisconnected?: () => void;
  onHandshakeRejected?: (reason: string) => void;
  onConnectError?: (error: string) => void;
  onCpEvent?: (event: CpEvent) => void;
};

/// Subscribe to all `cp:*` Tauri events at once. Returns an unsubscribe
/// fn that detaches every listener.
export async function subscribeCp(handlers: EventHandlers): Promise<UnlistenFn> {
  const unlisteners: UnlistenFn[] = [];
  if (handlers.onConnected)
    unlisteners.push(await listen("cp:connected", handlers.onConnected));
  if (handlers.onDisconnected)
    unlisteners.push(await listen("cp:disconnected", handlers.onDisconnected));
  if (handlers.onHandshakeRejected)
    unlisteners.push(
      await listen<string>("cp:handshake-rejected", (e) =>
        handlers.onHandshakeRejected!(e.payload),
      ),
    );
  if (handlers.onConnectError)
    unlisteners.push(
      await listen<string>("cp:connect-error", (e) =>
        handlers.onConnectError!(e.payload),
      ),
    );
  if (handlers.onCpEvent)
    unlisteners.push(
      await listen<CpEvent>("cp:event", (e) => handlers.onCpEvent!(e.payload)),
    );
  return () => unlisteners.forEach((u) => u());
}
