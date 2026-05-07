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

## What goes away (all DONE in Phase 4)

- `lutin-desktop-egui/` — the egui chrome (renamed during Phase 1,
  deleted in Phase 4).
- `crates/lutin-workflow-ui` — the cdylib trait surface. Replaced by
  a JSON/IPC contract.
- `crates/lutin-ui` — egui widget kit.
- `workflows/chat/src/ui.rs` — the chat cdylib UI. Replaced by an
  HTML/JS bundle in `workflows/chat/ui/`.
- `workflows/chat`'s `cdylib` crate-type, the `lutin.workflow.cdylib`
  Docker label, and CP's `read_cdylib_bytes` / `GetWorkflowCdylib` /
  `ResponseOk::WorkflowCdylib`. Replaced by the bundle-tarball
  equivalent.

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

### Phase 2: Plugin loading without native APIs — **DONE (modulo live-run smoke test)**

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

- **JS postcard codec** — DONE. `workflows/chat/ui/src/postcard.ts`
  implements the LEB128 varint + length-prefixed primitives postcard's
  standard flavour uses; `chat.ts` wires up `ChatRequest` (encode),
  `ChatResponse` / `ChatEvent` (decode). Shared with future plugins
  by promotion to a workspace-level helper if a second plugin lands;
  for now it's plugin-local since chat is the only consumer.
- **Real chat React UI** — DONE. `workflows/chat/ui/src/App.tsx` plus
  `session.ts` (a pure reducer mirroring `apply_chat_event` /
  `apply_chat_response` from the egui path). Composer, scrollback,
  persona indicator, cancel-while-streaming, error banner. Subscribe
  fires once on mount; broadcasts feed the reducer.
- **Wire-format pinned with golden tables.** Rust side:
  `tests::golden_postcard_bytes` in `workflows/chat/src/lib.rs`. JS
  side: `workflows/chat/ui/src/golden.test.ts` (`bun test`). Both
  hold the same byte-level expectations across `ChatRequest`,
  `ChatEvent`, and `ChatResponse` shapes; either side drifting trips
  one of the two tests immediately.
- **Iframe error propagation.** `lutin.ts::request` now rejects when
  chrome posts `{ kind: "response", request_id, error }`, instead of
  resolving with `undefined` and leaving callers hanging.
- **Image builds clean** — DONE. `docker build -f workflows/chat/Dockerfile
  -t lutin-workflow-chat:dev .` produces an image carrying both the
  engine binary and the bundle tarball. End-to-end live run is the
  remaining manual handoff: start CP pointing at the chat image, run
  desktop, open a chat session, verify a streamed reply renders.
  Nothing in code is gating that demo.
- **Cross-origin chrome-hosted shim** — DONE. New `lutin-shim` URI
  scheme handler in `lutin-desktop/src-tauri/src/shim_protocol.rs`
  serves a single embedded JS file (`shim/lutin.js`) with permissive
  CORS. Plugin `index.html` loads it via
  `<script src="lutin-shim://localhost/shim.js">` before any bundled
  JS runs; the shim sets `window.__lutinReady` so plugin code awaits
  the global instead of bundling its own copy. `workflows/chat/ui/src/
  lutin.ts` is now types + a thin global accessor only.
- **Permission enforcement gates** — DONE. `<PluginIframe>` builds a
  `permissions` Set from `opened.manifest.permissions` and rejects
  capability calls to undeclared perms before any side-effect runs.
  Today only `notification` is gated; `audio`/`hotkey`/`clipboard`
  hook into the same gate when Phase 3 lands. No origin → plugin_id
  registry is needed because each iframe gets its own MessagePort
  bound by closure to its manifest — port possession is the proof of
  identity, stronger than origin matching.

Phase 2 deliverable per the original plan was "end-to-end chat
streaming through chrome's bytes pump". The wire is fully open and
all code paths between iframe and engine compile + pass golden
tests. Live-run verification (real LLM, real WS) is a manual step
gated only on a configured CP and credentials.

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

### Undocumented Phase 2 additions

These shipped alongside Phase 2 but were not in the original plan
narrative. Listed here so future-you doesn't think they appeared from
nowhere:

- **CP session lifecycle**: `lutin-control-panel/src/session_index.rs`
  (per-workflow on-disk session index), `session_summary.rs` (reads
  workflow-written `summary.json` for list decorations),
  `settings_io.rs` (provider list persistence without clobbering other
  settings sections), updates to `defaults.rs`.
- **CP request surface**: `ResumeSession`/`SessionResumed` are wired
  through and used by the desktop's `workflow_session_open` fallback
  (see `lutin-desktop/src-tauri/src/lib.rs:230-247`).
- **Personas**: `personas/orchestrator.toml`, `personas/researcher.toml`
  added; `personas/assistant.toml` updated. Read by chat workflow for
  the persona picker.
- **Desktop chrome**: `lutin-desktop/src/components/TopBar.tsx` and
  `theme.css` for app-level chrome (header + global CSS variables).
- **Chat UI extras**: `workflows/chat/ui/src/PersonaComposer.tsx`
  (persona picker), `adapter.ts` (separated from `chat.ts`).

These don't change the Phase 2 architectural claims — they're feature
work that grew up around the migration. Phase 2's iframe + bytes-pump
contract is intact and load-bearing for everything below.

### Phase 3a: Keybinds + transcription — **DONE (slices 1–5; slice 6 deferred)**

Slices 1–5 landed, plus a listening/transcribing overlay window that
wasn't in the original plan. End-to-end PTT works: hotkey (X11 via
`global-shortcut`, Wayland via `xdg-desktop-portal-globalshortcuts`) →
mic capture → Whisper transcription → text delivered into the active
workflow iframe (or clipboard fallback). See per-slice notes below for
the actual implementation shape.


#### Goal

PTT working end-to-end: hotkey (global, even when app unfocused) →
mic capture → transcription → text delivered into the active workflow
iframe. Foundation built so wake-word / open-mic / dictate-into-focused
can be added later without restructuring.

Not in this phase: wake-word, dictate-into-focused-app (needs `enigo`),
voice-command parsing (port from old app's `voice_command.rs`).

#### Mental model (the partition)

- Hotkey registration, mic capture, transcription all live in **Rust
  core**. Single owner of mic and OS-level keys; iframes can't reach
  either.
- Workflows declare a *capability to be a target of transcribed audio*;
  they do **not** register or own combos. The user binds combos in
  chrome settings.
- A binding is `(combo, action, target)`. `action` selects the audio
  pipeline shape (PTT-style hold vs tap). `target` selects where the
  resulting text goes.
- Trigger source (hotkey now, wake-word later) is orthogonal to target.
  The same routing layer serves both.

#### Settings schema (extension to `DesktopSettings`)

```rust
pub struct DesktopSettings {
    // existing
    pub default: String,
    pub connections: Vec<ConnectionProfile>,
    // new
    pub keybinds: Vec<KeyBind>,
}

pub struct KeyBind {
    pub combo: String,          // accelerator string, e.g. "Numpad1",
                                // "CommandOrControl+Shift+M"
    pub action: Action,
    pub target: Target,
}

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    Ptt,            // hold-to-talk: down=start capture, up=stop+transcribe
    Dictate,        // tap to start, second tap (or silence) to stop
    // reserved (additive): OpenMicToggle, ChromeAction { name: String }
}

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    ActiveWorkflow,                          // route to focused session
    Workflow { workflow: WorkflowId },       // pinned workflow
    Clipboard,
    // reserved (additive): TypeIntoFocused
}
```

In-memory we keep an `Arc<Vec<KeyBind>>` plus a derived `combo →
KeyBind` lookup map. On `settings_set`, diff old/new combo sets and
re-register accordingly.

#### Tauri command surface

```rust
#[tauri::command]
fn keybind_combos_in_use(state) -> Vec<String>;
// for the settings UI to flag conflicts before commit

#[tauri::command]
fn set_active_session(state, session: Option<SessionId>);
// chrome calls on session switch / focus change so Rust can resolve
// Target::ActiveWorkflow
```

No `keybind_register` from JS. Workflows are *targetable*, not
*registrants*.

#### Hotkey dispatch

`tauri-plugin-global-shortcut` registers each `combo` at startup +
on settings change. Single global handler:

1. lookup `KeyBind` by combo,
2. spawn `dispatch(Trigger::Hotkey, action, target, phase)` on the
   tokio runtime (don't block the keyboard thread),
3. `phase` = `Down`/`Up` from the plugin's `ShortcutEvent::state`.

`Trigger` is an internal enum (`Hotkey | WakeWord` later). Routing
code only sees `Trigger`, so adding wake-word is purely additive.

#### Audio capture (`src-tauri/src/audio.rs`)

```rust
pub struct Capture { /* ... */ }
impl Capture {
    pub fn start(&self) -> Result<()>;      // idempotent
    pub fn stop(&self) -> Result<Vec<f32>>; // 16 kHz mono, drained
}
```

cpal default input device, resampled to 16 kHz mono (mirror old app's
`audio/capture.rs`). Single global capture instance; concurrent triggers
while held coalesce to one stream. In-memory buffer for v1; swap for a
chunk channel when streaming transcription is needed.

#### Transcription (`src-tauri/src/transcribe.rs`)

```rust
pub trait Transcriber: Send + Sync {
    async fn transcribe(&self, pcm_16k_mono: &[f32]) -> Result<String>;
}
pub struct StubTranscriber;     // returns "[transcribed N samples]"
pub struct WhisperTranscriber;  // slice 4: whisper-rs
```

Slices 1–3 ship `StubTranscriber` so the pipeline runs without model
plumbing. Slice 4 swaps in `WhisperTranscriber` behind the trait.

#### Routing (Rust → workflow iframe)

After `transcribe()` resolves with `text`:

- `Target::Clipboard` → `arboard::set_text(text)`.
- `Target::Workflow { workflow }` → resolve a session id for that
  workflow (heuristic: most-recent active session for that workflow id;
  if none, fall through to clipboard).
- `Target::ActiveWorkflow` → use `active_session_id`; if `None`, fall
  through to clipboard with a warning notification (don't lose audio).

For workflow targets, emit Tauri event `transcription:<session_id>`
with `{ text, source: "ptt" }`.

`<PluginIframe>` (already keyed by session id, already MessagePort-based)
listens for this event and forwards over the port as a **new envelope
kind** sibling to the existing engine bytes:

```ts
// chrome → iframe MessagePort frame
{ kind: "transcription", text: string, source: "ptt" | "openmic" }
```

Crucially this is **not** an engine bytes-pump message — it doesn't
go through the engine WS. The bytes pump stays pure (engine bytes
only). The iframe shim dispatches `kind === "transcription"` to a
separate listener registry.

#### Workflow capability declaration

Extend `lutin.workflow.json`:

```json
{
  "entry": "index.html",
  "permissions": ["clipboard"],
  "capabilities": ["receive_transcription"],
  "display_name": "Chat",
  "icon": "💬"
}
```

- `permissions` = what the workflow can *do* (existing).
- `capabilities` = what the workflow can *be a target of* (new).

Chrome's settings UI populates the target dropdown from installed
workflows that declare `receive_transcription`. Chrome refuses to
deliver to workflows that don't declare it (defense-in-depth — the
shim just won't expose `onTranscription`, but Rust also gates the
event emission).

#### Shim API addition

```ts
window.lutin.onTranscription(
  cb: (msg: { text: string; source: "ptt" | "openmic" }) => void
): () => void; // unsubscribe
```

Only injected when the manifest declares `receive_transcription`.
Subscription dispatches off the same MessagePort handler that already
demuxes `kind: "broadcast" | "response" | "notification"`.

#### Active session tracking

React store gains `activeSessionId: string | null`. `<PluginIframe>`
sets it on mount + on visibility change (intersection observer or
explicit `onSessionFocus` from the parent tab manager). Tauri command
`set_active_session(session_id)` syncs it into Rust. `Target::ActiveWorkflow`
reads this value at dispatch time.

Phase 2 already drops/remounts iframe on session switch (lessons line
411), so mount-time set is sufficient for v1.

#### Chat UI integration

`workflows/chat/ui/src/App.tsx`: register `lutin.onTranscription`,
push text into the composer (don't auto-send). Per-binding `auto_send`
can be added to `KeyBind` later if the manual confirmation gets old.

#### Settings UI

New section under Settings:

- List existing keybinds: `combo · action · target · [×]`
- Add binding: select `action` → select `target` (dropdown filtered to
  valid targets per action) → click-to-capture combo (focused input
  listens `keydown`, builds the accelerator string, Esc cancels)
- Conflict check via `keybind_combos_in_use` before save
- v1: no edit; delete + re-add. Keeps the UI tiny.

#### Slicing (sequenced, each runnable)

1. **Schema + dispatch wiring** — DONE. Added `tauri-plugin-global-shortcut`,
   extend settings, register/unregister combos, emit a `keybind:fired`
   Tauri event for testing. Seed default `Numpad1 + Ptt +
   ActiveWorkflow` when `keybinds` is empty. No UI.
2. **Audio capture + stub transcription + clipboard target** — DONE.
   `audio.rs` (cpal default input, resampled to 16 kHz mono),
   `transcribe::StubTranscriber` proved the pipeline before slice 4
   replaced it.
3. **Active session tracking + iframe delivery** — DONE.
   `set_active_session`, `transcription:<session>` event, shim
   `onTranscription`, chat manifest declares
   `capabilities: ["receive_transcription"]`, `App.tsx` pushes text
   into the composer.
4. **Real transcription.** — DONE. `WhisperTranscriber` (whisper-rs,
   Vulkan-accelerated build) replaces the stub. Models live at
   `~/.lutin/models/whisper/` (matches the old desktop layout, so users
   keep their cached weights). `KNOWN_WHISPER_MODELS` mirrors the old
   list — `ggml-large-v3-turbo.bin` (default) and
   `ggml-distil-large-v3.bin`. First call lazy-downloads from HF; load
   + inference both happen on `spawn_blocking` so neither the keyboard
   thread nor the tokio runtime stalls. New Tauri commands
   `whisper_known_models` / `whisper_local_models` /
   `whisper_ensure_model` for the slice-5 settings UI; `WhisperConfig`
   (model/language/beam_size) is part of `DesktopSettings`. Startup
   spawns a warmup task so the first PTT skips the load cost.
5. **Settings UI** — DONE. `KeybindsPanel` in `SettingsView.tsx`:
   add/delete bindings, click-to-capture combo, conflict check via
   `keybind_combos_in_use`, backend banner showing whether plugin or
   portal backend is live.
6. **(Deferred) external targets + wake-word.** `enigo` for
   `TypeIntoFocused`, `ort` + `openWakeWord` as a second `Trigger`
   variant feeding the same routing.

#### Undocumented Phase 3a additions

These shipped with Phase 3a but weren't in the original plan:

- **Wayland portal backend** (`keybind_portal.rs`). On Wayland,
  `tauri-plugin-global-shortcut` can't see keys (no compositor API).
  Instead we register through `xdg-desktop-portal`'s
  `org.freedesktop.portal.GlobalShortcuts` interface; the user grants
  combos through their compositor's portal dialog. `lib.rs::build_keybind_backend`
  picks portal on Wayland, plugin elsewhere. JS finds out which is
  active via `keybind_backend()` so the settings UI can explain
  portal-mediated combo capture.
- **Listening/transcribing overlay** (`overlay.rs` + second Tauri
  webview labelled `"overlay"`). Floating pill that shows
  `Listening → Transcribing → Done/Error` during a PTT cycle.
  `HIDE_GENERATION` counter prevents stale auto-hide tasks from
  killing newer phases. `LAST_PHASE` is cached so the freshly-mounted
  overlay webview can pull its initial phase via
  `overlay_current_phase` instead of racing the first emit.
- **Capability gate** (`capability.rs` + `dispatch.rs`). Workflow
  manifests' `capabilities` set is evaluated at dispatch time, not at
  iframe construction — `Target::ActiveWorkflow` checks the active
  session's manifest before emitting `transcription:<session>` and
  falls through to clipboard with a warning notification when the
  target session can't receive.

#### Designed-for-future hooks (locked in slice 1)

- `Trigger` enum + dispatch signature `fn dispatch(trigger, action,
  target, phase)`. Wake-word lands as another `Trigger` variant.
- `Transcriber` trait. Whisper-rs is one impl; a future remote
  endpoint or model swap is local change.
- `Action` and `Target` enums have reserved variants. Adding
  `OpenMicToggle` / `TypeIntoFocused` is additive, no migration.
- Iframe envelope kind set is open: `transcription` joins `broadcast`,
  `response`, `notification` without renaming.

#### Open questions to resolve during slice 1

1. **Default binding strategy.** Implicit default (`Numpad1 PTT →
   ActiveWorkflow` if `keybinds` is empty) vs seed-on-first-run. Lean
   implicit — keeps `desktop.json` clean for users who don't customize.
2. **`Target::ActiveWorkflow` against incapable workflow.** Fall back
   to clipboard with a warning notification. Confirmed during design.
3. **Multi-binding to same action.** Two combos both bound to `Ptt`
   with different targets is allowed; lookup is by combo. No special
   handling.
4. **PTT semantics with no active session.** Clipboard fallback (so
   audio isn't lost). Decided.

### Phase 4: Polish + cleanup — **cleanup DONE; window state + updater deferred**

Cleanup landed:

- `crates/lutin-workflow-ui`, `crates/lutin-ui`, `lutin-desktop-egui`,
  and `workflows/chat/src/ui.rs` deleted. The chat crate no longer
  declares a `cdylib` crate-type and drops its `egui` + `lutin-workflow-ui`
  deps; `lib.rs` no longer re-exports a `ui` module.
- Control protocol drops `Request::GetWorkflowCdylib` /
  `ResponseOk::WorkflowCdylib`. CP drops `Command::GetWorkflowCdylib`
  / `fetch_workflow_cdylib` / `read_cdylib_bytes`. The
  `lutin.workflow.cdylib` Docker label is no longer parsed; bundle
  label is now required, not optional.
- Chat Dockerfile drops the `libchat.so` build-stage assertion + COPY
  + the `lutin.workflow.cdylib` LABEL.
- Workspace `members` no longer references `lutin-desktop-egui`;
  `[workspace.dependencies] lutin-workflow-ui` removed.

Still on the Phase 4 punch list (deferred to a later session):

- Window state persistence (size/position) via
  `tauri-plugin-window-state`.
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
    Cargo.toml              (engine [[bin]] + rlib [lib] for tests; no cdylib)
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

Phases 1, 2, 3a, and the cleanup half of Phase 4 are landed. Remaining
work, all additive:

1. **Window state persistence.** `tauri-plugin-window-state`. Main
   window only — the overlay window is positioned by `overlay.rs`.
2. **Auto-update.** Wire Tauri's built-in updater against whatever
   release pipeline ships first; needs an updater signing keypair and
   a manifest endpoint.
3. **WebKitGTK smoke test on Wayland.** Trackpad scroll quality is
   the known risk; if unbearable, the escape hatch is Chromium-via-CEF,
   but we hope to avoid that.

Things to remember, learned the hard way:

- **`tsc -b` writes `.js` next to source by default**; chat-ui uses
  plain `tsc` + `noEmit: true` in `tsconfig.json`.
- **Tauri serializes `Vec<u8>` as a JSON number array** in IPC. JS
  side converts at the boundary (`Array.from(uint8)` outbound,
  `Uint8Array.from(arr)` inbound). Hidden in `api.ts` helpers.
