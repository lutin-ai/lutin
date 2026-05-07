// Plugin-side bindings for the chrome-hosted shim. The shim itself
// lives in the desktop crate and is served from
// `lutin-shim://localhost/shim.js` — see `index.html`. By the time
// any bundled module runs, the shim has already attached
// `window.__lutinReady`, a Promise that resolves with the Lutin API
// once chrome's `lutin-init` postMessage lands.
//
// Plugins should never import a runtime shim from this file; we only
// re-export the types and a thin awaiter for the global so callers
// can stay strictly typed.

export interface PluginManifest {
  entry: string;
  permissions: string[];
  /// Capabilities this workflow opts into receiving from the chrome.
  /// Slice 3 uses `"receive_transcription"`.
  capabilities: string[];
  display_name: string;
  icon: string;
}

export interface TranscriptionMessage {
  text: string;
  source: "ptt" | "openmic";
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
  /// Only present when the workflow's manifest declares
  /// `receive_transcription` in `capabilities`. Subscribe to receive
  /// PTT / open-mic transcription deliveries routed by chrome.
  onTranscription?(cb: (msg: TranscriptionMessage) => void): () => void;
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
