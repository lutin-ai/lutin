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
//                  | { kind: "select-sub-agent", id }
//   iframe → chrome: { kind: "request", request_id, body }
//                  | { kind: "tts-call", request_id, method, args }
//                  | { kind: "notification", title?, body }
//                  | { kind: "sub-agents-update", agents }
//                  | { kind: "session-summary-update", summary }
// `tts-call` and `request` share the same request_id space + pending
// map; chrome replies with `{ kind: "response", ... }` for both.
(function () {
  if (window.__lutinReady) return;

  let resolveLutin;
  window.__lutinReady = new Promise(function (r) { resolveLutin = r; });

  function buildLutin(port, init) {
    let nextRequestId = 1;
    const pending = new Map();
    const broadcastHandlers = new Set();
    const transcriptionHandlers = new Set();
    // Only populated when the workflow declared `sub_agents` in its
    // manifest; otherwise the chrome filters out `select-sub-agent`
    // messages on the receive side too, so this set never sees a
    // dispatch.
    const selectSubAgentHandlers = new Set();

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
      if (msg.kind === "select-sub-agent") {
        // Chrome-driven sub-agent selection. Plugins that don't render
        // a sub-agent surface (most workflows) simply don't subscribe;
        // the message lands and is dropped.
        for (const h of selectSubAgentHandlers) h(msg.id);
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
    // Strings must match Rust `capability::*`.
    const hasReceiveTranscription = caps.indexOf("receive_transcription") >= 0;
    const hasTts = caps.indexOf("tts") >= 0;
    const hasSubAgents = caps.indexOf("sub_agents") >= 0;

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
      // Session-level live counters (ctx fill + cumulative provider
      // tokens). Always exposed — the chrome's sidebar wants them
      // for every workflow, so this isn't capability-gated. Plugins
      // call this once per `summaryUpdated` broadcast they observe;
      // there's no "clear back to not-yet-known" sentinel — a session
      // that hasn't seen a usage report simply doesn't post.
      publishSummary: function (summary) {
        port.postMessage({ kind: "session-summary-update", summary: summary });
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

    // Sub-agent surface: only expose when the manifest declares
    // `"sub_agents"`. Same shape as the TTS / transcription gates —
    // chrome filters incoming `select-sub-agent` deliveries on the
    // capability too, so a non-declaring workflow can't observe
    // chrome's selection state.
    if (hasSubAgents) {
      api.publishSubAgents = function (agents) {
        port.postMessage({ kind: "sub-agents-update", agents: agents });
      };
      api.onSelectSubAgent = function (cb) {
        selectSubAgentHandlers.add(cb);
        return function () { selectSubAgentHandlers.delete(cb); };
      };
    }

    // Same shape for TTS: only expose `lutin.tts` when the manifest
    // declares `"tts"`. Defense-in-depth — chrome also rejects any
    // `tts-call` envelope from a workflow that didn't declare it, so
    // even a hand-rolled iframe can't reach the Tauri commands.
    if (hasTts) {
      function ttsCall(method, args) {
        const request_id = nextRequestId++;
        return new Promise(function (resolve, reject) {
          pending.set(request_id, { resolve: resolve, reject: reject });
          port.postMessage({
            kind: "tts-call",
            request_id: request_id,
            method: method,
            args: args || {},
          });
        });
      }
      api.tts = {
        ensureBackend: function (backend) { return ttsCall("ensureBackend", { backend: backend }); },
        openStream: function (backend) { return ttsCall("openStream", { backend: backend }); },
        speak: function (streamId, text, opts) {
          // Speed is integer thousandths on the wire (`1000` = 1.0×).
          // Default to 1.0× when the workflow doesn't set one; clamp
          // happens Rust-side via `TtsSpeed::Deserialize`.
          const speed = opts && typeof opts.speed === "number" ? Math.round(opts.speed * 1000) : 1000;
          return ttsCall("speak", { streamId: streamId, text: text, speed: speed });
        },
        cancel: function (streamId) { return ttsCall("cancel", { streamId: streamId }); },
        closeStream: function (streamId) { return ttsCall("closeStream", { streamId: streamId }); },
      };
    }

    return api;
  }

  // Zoom is driven by the chrome via `webview.setZoom`, which the
  // engine propagates to all iframes natively. No plugin-side
  // counter-scaling needed — and the prior CSS `zoom` path was the
  // root cause of typing lag on Linux WebKitGTK.

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

  // Chrome → iframe focus request: when the user presses the
  // configured `focusWorkflow` chord (e.g. `i`), the chrome posts this
  // and we focus the iframe's primary composer. A plugin can override
  // by handling the message itself (event is observable before this
  // handler runs only if it registers a capture-phase listener); the
  // default behaviour is "focus the first textarea, else the first
  // text input".
  window.addEventListener("message", function (e) {
    if (!e.data || typeof e.data !== "object") return;
    if (e.data.type !== "lutin-focus-input") return;
    const target =
      document.querySelector("textarea:not([disabled])") ||
      document.querySelector('input[type="text"]:not([disabled])') ||
      document.querySelector("input:not([type]):not([disabled])");
    if (target && typeof target.focus === "function") target.focus();
  });

  // Iframe → chrome keydown forwarding. The chrome's window-level
  // app-keybind handler can't see keys absorbed by a cross-origin
  // iframe, so the shim forwards them. We skip while a text input or
  // contenteditable has focus — those keystrokes are real typing, not
  // navigation. Escape is the one exception: when typing, Escape
  // blurs the input (so the next key lands as nav) *and* forwards so
  // chrome clears any in-flight leader.
  function isTextTarget(el) {
    if (!el || el.nodeType !== 1) return false;
    const tag = el.tagName;
    if (tag === "INPUT") {
      const t = (el.getAttribute("type") || "text").toLowerCase();
      // Buttons, checkboxes, etc. don't consume keys as text.
      return t === "text" || t === "search" || t === "email" || t === "url" ||
             t === "tel" || t === "password" || t === "number";
    }
    if (tag === "TEXTAREA") return true;
    if (el.isContentEditable) return true;
    return false;
  }
  window.addEventListener("keydown", function (e) {
    const inText = isTextTarget(e.target);
    if (inText && e.key === "Escape") {
      try { e.target.blur && e.target.blur(); } catch (_) {}
      // fall through to forward so parent clearLeader runs.
    } else if (inText) {
      return;
    }
    try {
      parent.postMessage({
        type: "lutin-keydown",
        key: e.key,
        code: e.code,
        ctrlKey: !!e.ctrlKey,
        metaKey: !!e.metaKey,
        shiftKey: !!e.shiftKey,
        altKey: !!e.altKey,
      }, "*");
    } catch (_) { /* parent unreachable — fine */ }
  }, true);
})();
