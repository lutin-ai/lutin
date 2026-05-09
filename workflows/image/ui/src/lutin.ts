// Plugin-side bindings for the chrome-hosted shim. Mirrors the chat
// workflow's `lutin.ts` — see comments there for the full pattern.
// We only need the request/broadcast surface for Slice 3.

export interface PluginManifest {
  entry: string;
  permissions: string[];
  capabilities: string[];
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
  request(body: Uint8Array): Promise<Uint8Array>;
  onBroadcast(cb: (body: Uint8Array) => void): () => void;
  notify(body: string, title?: string): void;
}

declare global {
  interface Window {
    __lutinReady?: Promise<Lutin>;
    lutin?: Lutin;
  }
}

export function getLutin(): Promise<Lutin> {
  const ready = window.__lutinReady;
  if (!ready) {
    return Promise.reject(
      new Error(
        "lutin shim missing — chrome did not serve lutin-shim://localhost/shim.js",
      ),
    );
  }
  return ready;
}
