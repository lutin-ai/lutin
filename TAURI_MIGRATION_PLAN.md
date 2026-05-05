# Tauri migration plan

Replaces the current egui-based `lutin-desktop` + cdylib-loaded workflow UIs
with a Tauri shell whose per-session UI is rendered in a webview from
HTML/JS/TS bundles shipped inside each workflow's Docker image.

## Why we're doing this

The dlopen'd-cdylib UI model is unsound on top of any Rust UI library, not
just egui. Empirically verified during the egui debug session:

```
[desktop]      typeid_probe = (..u64.., 0x22ba5a93008049bea65470fe5f8c3036, ..)
[chat-cdylib]  typeid_probe = (..u64.., 0x00a3836d08fbed58512157a1261a6441, ..)
```

`TypeId` for `egui::Context` differs across the FFI boundary because cargo
metadata hashes diverge between the two workspaces, even with bit-identical
egui sources. This breaks every `Any`-based API (egui plugins, tokio runtime
context lookups, anymap state). Each cdylib also has its own copy of every
Rust crate's static state. We hit this twice already (tokio TLS panic →
`Handle::spawn` UB → egui plugin lookup miss → `SmallVec` UB on drop).

Switching plugin → webview-with-JS-bundle removes the FFI sharing entirely:
plugin and chrome only exchange JSON over `postMessage`/IPC, no shared
Rust types.

## Target architecture

```
┌──────────────────────────────────────────────────────────┐
│  Tauri app (lutin-desktop)                               │
│  ┌────────────────────────────────────────────────────┐  │
│  │  Main webview: React app (chrome UI)               │  │
│  │  ┌──────────────────────────────────────────────┐  │  │
│  │  │  per-session iframe                          │  │  │
│  │  │  src=tauri://localhost/plugins/<id>/         │  │  │
│  │  │  (workflow-supplied HTML/JS bundle)          │  │  │
│  │  └──────────────────────────────────────────────┘  │  │
│  └────────────────────────────────────────────────────┘  │
│           ▲ Tauri IPC (commands + events)                │
│           │                                              │
│  ┌────────┴───────────────────────────────────────────┐  │
│  │  Rust core (Tauri commands)                        │  │
│  │  - audio capture (cpal)                            │  │
│  │  - wake-word detection (ONNX runtime)              │  │
│  │  - global hotkeys (global-hotkey)                  │  │
│  │  - clipboard, notifications                        │  │
│  │  - CP WebSocket client (existing lutin-control-…)  │  │
│  │  - workflow plugin bundle cache                    │  │
│  │  - per-session WS bridge to engines                │  │
│  └────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────┘
          ▲ ws://                  ▲ ws:// per session
          │                        │
   ┌──────┴──────┐         ┌───────┴──────┐
   │  CP         │         │  workflow    │
   │             │         │  engine      │
   │             │         │  container   │
   └─────────────┘         └──────────────┘
```

## What stays as-is

- `lutin-control-panel` — CP, project lifecycle, session orchestration.
- `lutin-control-protocol` — WS protocol between desktop and CP.
- `lutin-session-protocol`, `lutin-protocol` — frame envelope, session
  request/response shapes.
- All workflow **engines** (the Docker image binary that runs server-side
  per session): `workflows/chat/src/engine.rs` and the `chat`/`lutin-*`
  protocol crates it depends on. Engines speak the existing WS protocol;
  nothing about them changes.
- `lutin-storage`, `lutin-ids`, `lutin-keypair`, `lutin-auth`,
  `lutin-tools`, `lutin-llm`, `lutin-entities`, `lutin-agent-sdk`,
  `lutin-settings`, `lutin-workflow-sdk` — all unaffected.
- The `cp` client module in desktop (`lutin-desktop/src/cp.rs`) and the
  WS bridge (`lutin-desktop/src/bridge.rs`) — keep, they're protocol code,
  not UI.

## What goes away

- `lutin-desktop/src/app.rs`, `view/`, `settings.rs` (UI parts) — the
  egui chrome.
- `lutin-desktop/src/loader.rs` — the cdylib loader.
- `crates/lutin-workflow-ui` — the cdylib trait surface. Replaced by a
  JSON/IPC contract.
- `crates/lutin-ui` — egui widget kit. Gone.
- `workflows/chat/src/ui.rs` — the chat cdylib UI. Replaced by an HTML/JS
  bundle in `workflows/chat/ui/`.
- `workflows/chat`'s `cdylib` crate-type, the `lutin.workflow.cdylib`
  Docker label, and CP's `read_cdylib_bytes` / `GetWorkflowCdylib` on the
  cdylib path. Replaced by a bundle-tarball equivalent.

## Tech choices (locked in to avoid relitigation)

- **Tauri 2** (current major). Plugin system, multi-webview, mature.
- **JS package manager**: **pnpm** if Windows is in scope, **Bun**
  otherwise. Bun is faster but its Tauri/Windows story is less
  battle-tested.
- **Build**: Vite + **React 19** + TypeScript.
- **Iframes** for per-session plugin UIs (not separate Tauri windows).
  Communication via `postMessage`. See "Plugin isolation model" below
  for origin/handshake details — *don't* skim that section.
- **CSS / components**: **shadcn/ui** (Radix + Tailwind v4) is the
  default; if you decide against Tailwind, Radix primitives + plain
  CSS modules is the fallback. Pick day 1 — switching later means
  rewriting every component.
- **Native libs (Rust side)**:
  - `cpal` — cross-platform audio capture.
  - `global-hotkey` — global keybinds.
  - `arboard` — clipboard.
  - Wake-word: start with **openWakeWord** running via `ort` (ONNX
    Runtime Rust binding). `porcupine` is the alternative (tiny, fast,
    paid licence beyond personal use).
  - `notify-rust` — desktop notifications.
  - Local transcription (Phase 3): `whisper-rs`.
- **Workflow bundle format**: tarball of static assets, entry point
  declared in `lutin.workflow.json` (see Plugin manifest below). The
  Docker label `lutin.workflow.bundle` points at the tarball path
  inside the image.

## Plugin isolation model (read this before designing IPC)

Each plugin iframe runs on its **own origin**, separate from the chrome
React app's origin. This is non-negotiable — sharing an origin defeats
the iframe sandbox (any plugin script could touch chrome state via
`window.parent`). Concretely:

- Chrome React app: served at the Tauri default origin.
- Each plugin: served at a per-plugin origin via a Tauri **custom
  protocol** registered at startup (e.g. `plugin-<id>://localhost/`).
  The exact URL form differs by platform on Tauri 2 — Mac/Linux use
  `protocol://localhost/...`, Windows uses `https://protocol.localhost/...`.
  Look up the current Tauri 2 custom-protocol API before committing
  the URL strings; the registration is the part that matters, the
  string format is bookkeeping.

Cross-origin means **postMessage origin checks are mandatory** — both
sides verify `event.origin` against an expected value before trusting
any message. Chrome maintains a registry mapping `origin → plugin_id`
populated when it constructs each iframe.

### Shim delivery

Tauri webview *init scripts* run in the outer webview (the chrome
React app), **not in nested iframes**. So the `window.lutin` shim
cannot be injected by Tauri's standard mechanism. Two viable options:

1. **MessagePort handoff (preferred).** Plugin's own `index.html`
   imports a small bootstrap module from the chrome's origin via
   `<script type="module" src="https://chrome-origin/_plugin_shim.js">`
   — works because cross-origin module imports are allowed when CORS
   permits. The bootstrap waits for chrome's first `postMessage`
   carrying a `MessagePort`, then exposes `window.lutin` backed by
   that port. Chrome enforces permissions per-port.
2. **Per-plugin shim served alongside the bundle.** Chrome injects an
   extra script tag into each plugin's `index.html` at extract time,
   pointing at a shim file it also serves on the plugin origin. More
   plumbing, fragile (HTML rewriting), avoid unless option 1 hits a
   wall.

Go with option 1.

### Permission enforcement

Permissions are enforced by **chrome's Rust core**, not by the JS shim.
Every Tauri command call from a plugin's MessagePort is matched
against the plugin's manifest permissions before any native side-effect
runs. The JS shim only exposes methods for declared permissions as a
convenience — a hostile plugin that ignores the shim and posts raw
command messages still gets rejected by Rust.

### Token handling

Session WS tokens stay in Rust core. The JS bridge sees opaque
session ids, never tokens. When chrome sets up an engine bridge for
a session, it holds the token internally and pumps decoded payload
bytes (not raw frames) to/from the iframe.

## Workflow plugin contract

A workflow ships a static asset bundle in its Docker image. Inside the
bundle, the plugin's `index.html` imports the chrome's plugin
bootstrap, which exposes `window.lutin` once the parent hands over a
MessagePort. All plugins see the same API surface; the bootstrap is
hosted by chrome so updates land in one place.

### The `lutin` JS API (provided by chrome)

```ts
declare global {
  interface Window {
    lutin: {
      // Session context
      readonly project: string;     // slug
      readonly session: string;     // session id
      readonly workflow: string;    // workflow id

      // Workflow-engine I/O (proxied by chrome over the existing WS)
      send(bytes: Uint8Array): void;
      onMessage(cb: (bytes: Uint8Array) => void): () => void; // unsubscribe

      // Native capabilities (subset; chrome decides what to forward
      // based on workflow manifest declarations)
      audio?: {
        startCapture(opts?: {sampleRate?: number}): Promise<void>;
        stopCapture(): Promise<void>;
        onChunk(cb: (pcm: Float32Array) => void): () => void;
      };
      hotkey?: {
        register(combo: string): Promise<number>; // returns id
        unregister(id: number): Promise<void>;
        onTriggered(cb: (id: number) => void): () => void;
      };
      clipboard?: {
        copy(text: string): Promise<void>;
        readText(): Promise<string>;
      };
      notification: {
        post(body: string, opts?: {title?: string}): void;
      };

      // Chrome chrome (sic) — ask chrome to do app-level things
      activateSession(session: string): void;
      startSession(workflow: string): void;
    }
  }
}
```

The bytes-in/bytes-out interface stays the same shape as today's
`Transport`. Plugin authors use whatever encoding their engine uses
(postcard, JSON, MessagePack — chrome doesn't care).

### Plugin manifest

Inside the bundle, an `lutin.workflow.json`:

```json
{
  "entry": "index.html",
  "permissions": ["audio", "hotkey", "clipboard"],
  "display_name": "Chat",
  "icon": "💬"
}
```

Chrome reads this, decides what subset of the `lutin.*` API to inject
into the iframe, and uses `display_name`/`icon` for the project chrome
(replaces the Docker labels we just added).

## Tauri command surface (Rust side, pseudocode)

```rust
#[tauri::command] async fn cp_send(req: ProtocolRequest) -> Result<...>;
#[tauri::command] async fn cp_subscribe(state: State<'_, AppState>) -> Channel<CpEvent>;

#[tauri::command] async fn workflow_open_session(slug: Slug, workflow: WorkflowId) -> SessionInfo;
#[tauri::command] async fn workflow_send(session: SessionId, bytes: Vec<u8>);
#[tauri::command] async fn workflow_subscribe(session: SessionId) -> Channel<Vec<u8>>;

#[tauri::command] async fn audio_start_capture(opts: AudioOpts) -> Result<()>;
#[tauri::command] async fn audio_stop_capture() -> Result<()>;
// audio chunks emitted via global event "audio:chunk"

#[tauri::command] async fn wake_word_register(phrase: String) -> Result<u32>;
// fires "wake-word:triggered" event with phrase id

#[tauri::command] async fn hotkey_register(combo: String) -> Result<u32>;
// fires "hotkey:triggered" event with hotkey id

#[tauri::command] async fn clipboard_copy(text: String) -> Result<()>;
#[tauri::command] async fn clipboard_read() -> Result<String>;

#[tauri::command] async fn notify(body: String, title: Option<String>) -> Result<()>;
```

Plugin iframes don't get direct access to these — chrome proxies a
narrowed subset based on the plugin's declared permissions. Chrome itself
(the React app) uses them directly.

## Migration phases

Each phase ends in a runnable state so we can stop at any of them.

### Phase 1: Tauri skeleton, no plugins yet — **DONE**

Landed in `06ba305` + `8e8eeff`. Tauri standard layout under
`lutin-desktop/{src,src-tauri}`, CP client wrapped in Tauri commands
(`cp_send`, `cp_status`, `settings_get`, `settings_set`), React chrome
with project list / create / delete / session tabs / settings, and
session pane placeholder. `cp_status` returns a full `ConnSnapshot`
(externally tagged JSON) so the React side hydrates without racing
against the event listener attaching.

### Phase 2: Plugin loading without native APIs — **MOSTLY DONE**

Done so far:

- **CP** (`3e32827`): `Request::GetWorkflowBundle` + `Response::Ok::WorkflowBundle`,
  `lutin.workflow.bundle` Docker label, `read_bundle_bytes` parallel to
  `read_cdylib_bytes`. `inspect_image` accepts either the cdylib OR the
  bundle label (not both required) so images can ship just the bundle
  during transition. `lutin-desktop-egui` removed from workspace
  members so its exhaustive `Response` match doesn't gate rebuilds.
- **Bundle cache + plugin protocol** (`1d4c6d0`):
  - `bundles::BundleCache` keyed by `(workflow_id, digest)`. Unpacks
    tarballs under `app_cache_dir()/bundles/<id>/<short_digest>/`.
    Path-traversal hardened.
  - `plugin_protocol::SCHEME = "lutin-plugin"`, single scheme with the
    workflow id in the **host** position (`lutin-plugin://chat/...`)
    so each plugin gets a distinct browser origin. Windows/non-Windows
    URL form hidden behind `plugin_protocol::url_for`.
  - `workflow_open_plugin` Tauri command: ensures bundle unpacked
    (fetches `GetWorkflowBundle` on cache miss), parses
    `lutin.workflow.json`, returns `{ url, manifest }`.
  - React `<PluginIframe>` resolves the URL, loads a sandboxed iframe,
    handles cross-origin postMessage with the chrome origin.
- **Engine bridge + bytes pump** (`5b75d45`):
  - `bridge.rs`: per-session WS pump. Holds the session token (never
    crosses to JS), runs Hello/HelloAck once, allocates `request_id`
    for `Frame::Payload`, correlates replies via a pending map, fans
    `Frame::Broadcast` bodies out to a `Vec<Channel<Vec<u8>>>` of
    subscribers and drops dead ones on the next send. Ping/Pong stays
    in-bridge.
  - Tauri commands: `workflow_session_open` (calls CP `OpenSession`,
    dials engine, stashes `BridgeHandle` keyed by session id),
    `workflow_session_request`, `workflow_session_subscribe` (Tauri
    `Channel<Vec<u8>>`), `workflow_session_close`. Token never
    surfaces to JS.
  - `cp_dispatch` helper extracted so Rust commands can call CP
    without round-tripping through the JS invoke layer.
  - `<PluginIframe>` proxies the bytes pump: iframe → chrome envelope
    is `{ kind: "request" | "response" | "broadcast" | "notification",
    request_id?, body }`. Chrome strips/wraps the `Frame` envelope so
    iframes only see body bytes; chat & friends still ride postcard
    end-to-end on those bytes.
- **Chat plugin scaffolding** (`a348e9b`): `workflows/chat/ui/` with
  React + Vite + bundled `lutin.ts` shim. Two-stage Dockerfile (UI
  builder via `oven/bun`, tar at `/workflow/ui.tar`); image now ships
  both `lutin.workflow.cdylib` and `lutin.workflow.bundle`. UI is a
  stub that completes the handshake and renders session context.

What's left for Phase 2:

- **JS postcard codec** for chat's protocol (`ChatRequest`,
  `ChatResponse = Result<ChatOk, ChatError>`, `ChatEvent`). No
  canonical JS port exists. Options, in order of preference:
  1. Hand-roll the postcard subset chat needs (varint, length-prefixed
     strings, enum tags, `Vec<T>`, `Option<T>`). Manageable for the
     chat surface; share as `workflows/chat/ui/src/postcard.ts` first,
     extract to a workspace-level helper if a second plugin needs it.
  2. Ship a translation layer in chrome that JSON-ifies the chat
     protocol on the iframe boundary. Keeps the engine untouched but
     means chrome grows chat-specific knowledge — bad fit for a
     generic chrome.
  3. Switch chat's wire format to JSON on a feature flag. Touches the
     engine — explicitly excluded by the plan.
  Go with (1).
- **Real chat React UI**: composer, scrollback, persona indicator —
  mirror today's egui surface (`workflows/chat/src/ui.rs`). The shim
  is the only async boundary; rendering is plain React state driven
  off `lutin.onBroadcast` events.
- **Cross-origin chrome-hosted shim** (deferred but planned): plugins
  currently ship their own copy of `lutin.ts`. Move it to a chrome-
  served file (e.g. via a `lutin-shim` URI scheme handler that adds
  `Access-Control-Allow-Origin: *`) so updates land in one place.
- **Permission enforcement gates**. Today the iframe's
  `notification.post` runs unconditionally chrome-side; once Phase 3
  Tauri commands land (audio, hotkey, clipboard), chrome must check
  the calling iframe's manifest before forwarding. The origin →
  plugin_id registry is implicit today (host segment of the iframe
  URL); make it explicit when the first capability ships.

Phase 2 deliverable per the original plan was "end-to-end chat
streaming through chrome's bytes pump". The wire is fully open;
postcard-in-JS is the gating item between here and that demo.

### Phase 2 lessons + decisions to lock in

- **Iframe boundary is body bytes, not Frames.** Chrome strips the
  `Frame::{Payload, Broadcast}` envelope; iframes only ever see
  `body` bytes. Rationale: iframes don't need a JS postcard impl just
  to peel the envelope, and chrome already owns request_id allocation.
  This diverges slightly from `lutin-workflow-ui::Transport`, which
  forwards full Frames — keep that contract for the legacy egui path
  but don't replicate it in the iframe shim.
- **Single `lutin-plugin` scheme, workflow id in host.** Cross-origin
  isolation falls out of the host comparison; one scheme registration
  covers every plugin. Don't register one scheme per plugin.
- **Tauri Channel<T> for broadcast subscribers.** Each
  `workflow_session_subscribe` invocation gets its own channel; the
  bridge drops dead ones on the next send. Avoids needing a global
  Tauri event channel for per-session broadcasts.
- **Drop iframe on session switch.** `<PluginIframe>` is keyed by
  session id, so switching tabs unmounts/remounts and the bridge
  reopens. Acceptable cost (one Hello roundtrip); avoids sticky-state
  bugs from session reuse.
- **Ship cdylib + bundle in parallel.** Don't dual-source-of-truth in
  the workflow code — chat's egui `ui.rs` keeps existing untouched
  until Phase 4 cleanup; the bundle is its own source under `ui/`.

### Phase 3: Native capabilities

- Add Tauri commands for audio capture (cpal), wake-word (ort +
  openWakeWord), global hotkeys, clipboard, notifications.
- Extend the JS shim with the corresponding subset, gated on plugin
  manifest permissions.
- Build a chrome-level "global transcription" pane (not per-plugin):
  wake word fires → start capture → run transcription → push text into
  active session's iframe via `lutin.send` or a higher-level
  `lutin.dictate(text)` helper.
- Decide where transcription model runs: locally (whisper.cpp via
  `whisper-rs`) or via the chat engine (already calling LLMs). Local is
  simpler for global-transcription latency.

### Phase 4: Polish + cleanup

- Delete dead code: `crates/lutin-workflow-ui`, `crates/lutin-ui`, all
  egui usage. Remove `lutin.workflow.cdylib` Docker label support from
  CP if Phase 2 left it as a dual-write.
- Window state persistence (size/position).
- Auto-update strategy (Tauri has built-in updater — wire it up against
  whatever release pipeline you settle on).
- Linux WebKitGTK: smoke-test on Wayland. If trackpad scrolling is
  unbearable, evaluate Chromium-via-CEF as an escape hatch.

## Repository layout after the dust settles

```
crates/
  lutin-control-protocol/   (unchanged)
  lutin-session-protocol/   (unchanged)
  lutin-protocol/           (unchanged)
  lutin-storage/            (unchanged)
  lutin-ids/                (unchanged)
  lutin-keypair/            (unchanged)
  lutin-auth/               (unchanged)
  lutin-llm/                (unchanged)
  lutin-tools/              (unchanged)
  lutin-entities/           (unchanged)
  lutin-agent-sdk/          (unchanged)
  lutin-settings/           (unchanged)
  lutin-workflow-sdk/       (unchanged — this is engine-side, not UI)
  # gone: lutin-workflow-ui, lutin-ui

lutin-control-panel/        (unchanged)

lutin-desktop/              (rewritten — Tauri 2 standard layout)
  src/                      (React app via Vite — frontend)
  src-tauri/
    Cargo.toml              (Tauri deps, no eframe/egui)
    tauri.conf.json
    src/                    (Rust commands, CP client, bridge)

workflows/
  chat/
    Cargo.toml              (engine only — no [lib], no cdylib)
    src/
      engine.rs             (unchanged)
      lib.rs                (engine-side helpers; UI gone)
      ...
    Dockerfile              (no libchat.so, instead COPY ui/dist/ to /workflow/ui/)
    ui/                     (new: workflow UI bundle source)
      package.json
      vite.config.ts
      src/
        main.tsx
        ...
```

## Open questions to resolve at start of next session

1. **Linux primary?** If yes, we need a fallback plan for WebKitGTK
   trackpad scroll quality. Decide tolerance threshold up front.
2. **Bun vs pnpm.** Bun is fast and built-in test runner; pnpm is the
   safe ecosystem choice. Lean Bun unless someone has a reason.
3. **CSS**: Tailwind v4, vanilla CSS modules, or a component lib like
   shadcn/ui? Recommend shadcn/ui — gives us decent components without
   committing to a heavy framework.
4. **Where does global transcription live?** Local whisper.cpp vs
   round-trip through chat engine. Recommend local for latency.
5. **Wake-word lib**: openWakeWord (free, ONNX) vs porcupine (tiny,
   commercial-licence above personal). Recommend openWakeWord.
6. **State management in React app**: Zustand (small, idiomatic) vs
   plain context+reducer. Recommend Zustand.
7. **Multi-window?** Phase 1 ships single-window. Decide later if we
   want detachable session windows.

## What to do first when context is fresh

Phase 1 + most of Phase 2 are landed. To finish Phase 2:

1. **JS postcard codec** in `workflows/chat/ui/src/postcard.ts`
   covering chat's protocol surface. Reference is the Rust types in
   `workflows/chat/src/lib.rs` (`ChatRequest`, `ChatOk`, `ChatError`,
   `ChatEvent`, `SessionState`, `HistoricalMessage`, `TurnId`,
   `FinishReason`). Verify against postcard's varint format
   (`postcard::to_allocvec` with the standard flavour). Worth a
   round-trip golden test that loads a captured `Frame::Broadcast`
   body and asserts the JS decode matches.
2. **Real chat UI in `workflows/chat/ui/src/`** — composer, scrollback,
   persona indicator. State flows from `lutin.onBroadcast` callbacks
   into a small reducer; `lutin.request(encode(SendMessage))` for
   intents. Mirror the current egui shape (`workflows/chat/src/ui.rs`).
3. **Smoke test**: rebuild the chat image
   (`docker build -f workflows/chat/Dockerfile -t lutin-workflow-chat:dev .`),
   wire CP to it, start a chat session in the desktop chrome, send a
   message, see a streamed reply in the iframe.
4. **Cross-origin chrome-hosted shim** (optional but cleaner): move
   `lutin.ts` out of the chat bundle and serve it from chrome via a
   `lutin-shim://localhost/shim.js` scheme handler with permissive
   CORS. Plugin `index.html` imports it. One copy, no per-plugin
   drift.

Things to remember, learned the hard way:

- **`tsc -b` writes `.js` next to source by default**; chat-ui uses
  plain `tsc` + `noEmit: true` in `tsconfig.json`.
- **Tauri serializes `Vec<u8>` as a JSON number array** in IPC. JS
  side converts at the boundary (`Array.from(uint8)` outbound,
  `Uint8Array.from(arr)` inbound). Hidden in `api.ts` helpers.
- **Don't re-add `lutin-desktop-egui`** to the workspace members
  list until Phase 4 — it has an exhaustive `Response` match that
  trips on every new variant.
- `crates/lutin-workflow-ui`, `crates/lutin-ui`, `workflows/chat/src/ui.rs`
  remain on disk and compile via the chat crate's own workspace.
  Don't delete until Phase 4.
