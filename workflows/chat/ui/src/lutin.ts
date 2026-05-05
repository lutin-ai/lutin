// `window.lutin` shim — bundled with this plugin for now (cross-origin
// chrome-hosted version is a follow-up). The shim waits for chrome's
// initial `lutin-init` postMessage carrying the MessagePort plus
// session context, then exposes a request/response + broadcast API
// over the port.
//
// The chrome ↔ iframe envelope:
//   chrome → iframe: { kind: "response", request_id, body } |
//                    { kind: "broadcast", body }
//   iframe → chrome: { kind: "request", request_id, body } |
//                    { kind: "notification", title?, body }
//
// `body` is `Uint8Array` (postcard-encoded `Frame::Payload.body` /
// `Frame::Broadcast.body`) so workflows can keep using whatever
// encoding their engine speaks.

export interface PluginManifest {
  entry: string;
  permissions: string[];
  display_name: string;
  icon: string;
}

export interface LutinInit {
  slug: string;
  session: string;
  workflow: string;
  manifest: PluginManifest;
}

export interface Lutin extends LutinInit {
  /// Send a request body to the engine and resolve with the matching
  /// response body. Chrome allocates the request_id and correlates
  /// the reply.
  request(body: Uint8Array): Promise<Uint8Array>;
  /// Subscribe to engine broadcast bodies. Returns an unsubscribe fn.
  onBroadcast(cb: (body: Uint8Array) => void): () => void;
  /// Show a chrome-side notification (browser-level for now; OS-level
  /// once the Tauri command lands).
  notify(body: string, title?: string): void;
}

let resolveLutin: ((l: Lutin) => void) | null = null;
const lutinReady: Promise<Lutin> = new Promise((r) => { resolveLutin = r; });

window.addEventListener("message", (e: MessageEvent) => {
  if (!e.data || typeof e.data !== "object") return;
  if (e.data.type !== "lutin-init") return;
  const port = e.ports[0];
  if (!port) return;
  const init = e.data as { type: string } & LutinInit;
  resolveLutin?.(buildLutin(port, init));
  resolveLutin = null;
}, { once: true });

function buildLutin(port: MessagePort, init: LutinInit): Lutin {
  let nextRequestId = 1;
  const pending = new Map<number, (body: Uint8Array) => void>();
  const broadcastHandlers = new Set<(body: Uint8Array) => void>();

  port.onmessage = (e) => {
    const msg = e.data as
      | { kind: "response"; request_id: number; body: Uint8Array }
      | { kind: "broadcast"; body: Uint8Array }
      | undefined;
    if (!msg) return;
    if (msg.kind === "response") {
      const cb = pending.get(msg.request_id);
      if (cb) {
        pending.delete(msg.request_id);
        cb(msg.body);
      }
      return;
    }
    if (msg.kind === "broadcast") {
      for (const h of broadcastHandlers) h(msg.body);
    }
  };
  port.start();

  return {
    ...init,
    request(body: Uint8Array) {
      const request_id = nextRequestId++;
      return new Promise<Uint8Array>((resolve) => {
        pending.set(request_id, resolve);
        // Don't transfer the buffer — keeps `body` reusable on the
        // caller side, at the cost of a structured-clone copy.
        // Worth it: chat payloads are small and the surprise of a
        // detached buffer here would be sharp.
        port.postMessage({ kind: "request", request_id, body });
      });
    },
    onBroadcast(cb) {
      broadcastHandlers.add(cb);
      return () => broadcastHandlers.delete(cb);
    },
    notify(body, title) {
      port.postMessage({ kind: "notification", body, title });
    },
  };
}

export function getLutin(): Promise<Lutin> {
  return lutinReady;
}
