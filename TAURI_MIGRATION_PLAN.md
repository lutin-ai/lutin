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

### Phase 1: Tauri skeleton, no plugins yet

- **First action: verify `lutin-workflow-sdk` doesn't depend on
  `lutin-workflow-ui`.** Quick `grep -rn workflow_ui crates/lutin-workflow-sdk/`.
  If it does, untangle before starting (engine-side SDK shouldn't pull
  in any UI types).
- New `lutin-desktop` rewrite as a Tauri app. Keep crate name; replace
  guts. **Use Tauri's standard layout** (don't invent your own — every
  Tauri example assumes this):
  ```
  lutin-desktop/
    src/                      React app
      package.json
      vite.config.ts
      App.tsx, main.tsx, ...
    src-tauri/                Rust core
      Cargo.toml
      tauri.conf.json
      src/
        main.rs
        cp.rs                 (ported from current lutin-desktop)
        bridge.rs             (ported from current lutin-desktop)
        commands/             (Tauri command modules)
  ```
- Rust core wraps the existing `cp.rs` and `bridge.rs` in Tauri commands.
- React app: project list, create/delete project, settings panel
  (connection profiles), session tabs — but no workflow UI yet
  (placeholder).
- Verifies: connect to CP, list projects, create/start sessions, observe
  events. Basically reproduces today's chrome minus workflow rendering.
- Deliverable: app runs, can open a project, can `+ New` a chat session
  (lands on a placeholder pane saying "plugin loading not implemented").

### Phase 2: Plugin loading without native APIs

- CP-side: replace `lutin.workflow.cdylib` with `lutin.workflow.bundle`
  Docker label pointing at a tarball path inside the image (e.g.
  `/workflow/ui.tar`). New CP command `GetWorkflowBundle` (parallel to
  the existing `GetWorkflowCdylib`, which can stay during migration).
- Desktop: bundle cache analogous to `WorkflowCache`, keyed by
  `(workflow_id, digest)`, unpacks tarballs to disk under the user's
  cache dir.
- Tauri serves each plugin under its own custom protocol/origin (see
  "Plugin isolation model"). Look up the Tauri 2 custom-protocol API
  at the time you wire this up — URL forms differ by platform.
- React renders one iframe per session, `src` = the plugin's origin
  root.
- Chrome React app serves a plugin bootstrap module on its own origin;
  plugin's `index.html` imports it. After the iframe loads, chrome
  posts an initial `MessagePort` message that the bootstrap uses to
  back `window.lutin`. Initial surface: `send` / `onMessage` /
  `notification.post`.
- Origin → plugin_id registry on the Rust side: every command call
  validated against the calling iframe's plugin permissions.
- Workflow ↔ engine bytes pump: chrome maintains the existing per-session
  WS to the engine; iframe ↔ chrome via `postMessage`; chrome ↔ engine
  via the existing `bridge.rs` (or a slimmed equivalent).
- Chat workflow rewrite: `workflows/chat/ui/` directory with React app,
  Vite build, Dockerfile updated to copy `dist/` into `/workflow/ui/`
  and tar it up at build time (or just COPY the directory; bundler
  decision).
- Deliverable: end-to-end chat working in iframe, streaming from engine,
  via chrome's bytes pump.

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

1. **Audit `lutin-workflow-sdk`.** `grep -rn workflow_ui crates/lutin-workflow-sdk/`.
   If it imports anything UI-side, refactor before scaffolding the
   Tauri app — engine-side SDK should not depend on UI types.
2. **Scaffold via the Tauri CLI**, not `cargo new`. Use
   `pnpm create tauri-app@latest` (or `bun create tauri-app`). Pick
   React + TypeScript + Vite. Then port `cp.rs` / `bridge.rs` from
   today's `lutin-desktop` into the generated `src-tauri/src/`. Crate
   name in `src-tauri/Cargo.toml` stays `lutin-desktop`.
3. **Lock the iframe origin model day 1.** Implement the per-plugin
   custom-protocol registration *before* loading any real plugin —
   even with one stub plugin. Cross-origin postMessage retrofitting
   is painful.
4. **Phase 1 first commit goal**: scaffolded Tauri app, the
   `cp_connect` / `cp_send` / `cp_subscribe` commands working,
   project list rendering, create/delete/select project working. No
   plugin support yet (placeholder pane for the active session).
5. **Don't delete** `crates/lutin-workflow-ui`, `crates/lutin-ui`, or
   `workflows/chat/src/ui.rs` until Phase 2 lands. They're already
   broken but their absence will mask compilation issues elsewhere
   during the rebuild. Comment out the workspace members or remove
   them from the workspace `members` list to skip compilation
   without deleting.
