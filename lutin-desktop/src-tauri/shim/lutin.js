// Chrome-hosted plugin shim. Served at `lutin-shim://localhost/shim.js`
// with permissive CORS so any plugin iframe (each on its own origin)
// can `<script src=...>` it. Sets up `window.__lutinReady` — a Promise
// that resolves with the `Lutin` API object after chrome's first
// `lutin-init` postMessage carrying the MessagePort.
//
// Envelope (must match `PluginIframe.tsx`):
//   chrome → iframe: { kind: "response", request_id, body | error }
//                  | { kind: "broadcast", body }
//                  | { kind: "transcription", text, source }
//   iframe → chrome: { kind: "request", request_id, body }
//                  | { kind: "notification", title?, body }
(function () {
  if (window.__lutinReady) return;

  let resolveLutin;
  window.__lutinReady = new Promise(function (r) { resolveLutin = r; });

  function buildLutin(port, init) {
    let nextRequestId = 1;
    const pending = new Map();
    const broadcastHandlers = new Set();
    const transcriptionHandlers = new Set();

    port.onmessage = function (e) {
      const msg = e.data;
      if (!msg) return;
      if (msg.kind === "response") {
        const p = pending.get(msg.request_id);
        if (!p) return;
        pending.delete(msg.request_id);
        if ("error" in msg) p.reject(new Error(msg.error));
        else p.resolve(msg.body);
        return;
      }
      if (msg.kind === "broadcast") {
        for (const h of broadcastHandlers) h(msg.body);
        return;
      }
      if (msg.kind === "transcription") {
        // Defense-in-depth: chrome already gates on the manifest's
        // `capabilities`, but if a transcription envelope leaks
        // through against a workflow that didn't declare
        // `receive_transcription`, the handler set is empty so it's
        // dropped silently.
        const payload = { text: msg.text, source: msg.source };
        for (const h of transcriptionHandlers) h(payload);
      }
    };
    port.start();

    const caps = (init.manifest && init.manifest.capabilities) || [];
    // String must match Rust `capability::RECEIVE_TRANSCRIPTION`.
    const hasReceiveTranscription = caps.indexOf("receive_transcription") >= 0;

    const api = Object.assign({}, init, {
      request: function (body) {
        const request_id = nextRequestId++;
        return new Promise(function (resolve, reject) {
          pending.set(request_id, { resolve: resolve, reject: reject });
          port.postMessage({ kind: "request", request_id: request_id, body: body });
        });
      },
      onBroadcast: function (cb) {
        broadcastHandlers.add(cb);
        return function () { broadcastHandlers.delete(cb); };
      },
      notify: function (body, title) {
        port.postMessage({ kind: "notification", body: body, title: title });
      },
    });

    // Only expose `onTranscription` when the manifest opted in.
    // Plugins that didn't declare the capability won't even see the
    // method on `window.lutin` — type-level signal that they shouldn't
    // be receiving transcriptions.
    if (hasReceiveTranscription) {
      api.onTranscription = function (cb) {
        transcriptionHandlers.add(cb);
        return function () { transcriptionHandlers.delete(cb); };
      };
    }

    return api;
  }

  window.addEventListener("message", function (e) {
    if (!e.data || typeof e.data !== "object") return;
    if (e.data.type !== "lutin-init") return;
    const port = e.ports[0];
    if (!port) return;
    const init = {
      slug: e.data.slug,
      session: e.data.session,
      workflow: e.data.workflow,
      manifest: e.data.manifest,
    };
    const lutin = buildLutin(port, init);
    window.lutin = lutin;
    resolveLutin(lutin);
  }, { once: true });
})();
