// Plugin-side bindings for the chrome-hosted shim. The shim attaches
// `window.__lutinReady` before any bundled module runs; this file just
// awaits it. Mirrors the principled UI's shim awaiter, trimmed to the
// surface the scratchpad workflow uses (no TTS, no sub-agents).

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
