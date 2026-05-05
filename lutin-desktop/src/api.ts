import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  ConnState,
  CpEvent,
  DesktopSettings,
  Request,
  Response,
  ResponseOk,
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
