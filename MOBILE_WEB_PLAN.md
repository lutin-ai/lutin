# Mobile web app — implementation plan

A new mobile-first React PWA at `lutin-mobile/`, talking to the existing
CP and workflow-engine WebSocket servers. Not a Tauri-mobile target; not
a native app. Notifications later piggy-back on email/Telegram routed
from CP rather than APNs/FCM.

## Why a separate frontend (not responsive Tauri)

- Backend is already remote-first: CP (`lutin-control-panel/src/lib.rs:784-898`)
  and workflow engines (`workflows/chat/src/engine.rs:653+`) serve token-auth
  WebSockets. Desktop is just one client. Browser is another.
- `packages/chat-widgets` is framework-clean React, no Tauri imports —
  drop-in reusable.
- Tauri-mobile would still require redesigning the IA (sidebar + multi-pane
  is wrong for thumbs) AND fighting Tauri-mobile tooling. Worst of both
  worlds.

## Design direction

Chat-first (`mockups/03_chat_first.html`):

- Slim top bar: persona name + streaming dot + hamburger.
- Left drawer: search, "+ New session" → workflow picker, sessions
  grouped by recency, persona/settings footer.
- Bottom sheets for tool inspection and workflow picker.
- Full-screen push for settings.
- No bottom tab bar. No persistent chrome between user and composer.

Power-user follow-up (deferred): swipe-down command palette over the chat
surface for fuzzy search across sessions/workflows/personas/settings.

## App location & build

- Path: `lutin-mobile/` at repo root, sibling to `lutin-desktop/`.
  Workspace member (added to root `package.json`).
- Tooling: Vite + React 19 + TypeScript, `bun` package manager — mirrors
  `lutin-desktop` so `@lutin/chat-widgets` works without bundler friction.
- Routing: none. SPA, view state in zustand. Drawer/sheet/push are
  overlays, not routes.
- PWA: `vite-plugin-pwa` added in v2.

## Phasing

**v1 — usable chat client.** Sequential PRs:

0. ✅ Scaffold: workspace package boots, `chat-widgets` theme imports,
   placeholder renders. Done — see `lutin-mobile/` and the workspaces
   entry in root `package.json`.
1. `packages/lutin-ws-client/` — `Frame` codec (postcard) + browser
   `WebSocket` wrapper for CP + engine. Hello/HelloAck handshake,
   request/response correlation, broadcast event emitter.
2. Settings push view: paste `wss://host:port` + bearer token, persist
   to `localStorage`, "Test connection" button.
3. Connect to CP, list projects + sessions, render in drawer, apply
   broadcast events.
4. Open one session: spawn engine WS, render via `<ChatView>` from
   chat-widgets directly (no iframe), send/receive messages.
5. Mobile shell: top bar, drawer animations, bottom sheet primitive,
   tool detail sheet.
6. Composer with text + send. Mic button is a stub (disabled, "v2"
   tooltip).

**v2 — polish & extras.** PWA manifest + service worker (installable,
offline shell), workflow picker for non-chat workflows (iframe host),
QR pairing, swipe-down command palette, push notifications via
CP-routed channels, mic/STT via browser MediaRecorder → CP transcription,
plugin iframe hosting.

## v1 module breakdown (`lutin-mobile/src/`)

- `main.tsx` — React root, theme imports.
- `App.tsx` — top-level shell: `<TopBar>`, `<ChatSurface>`, `<Drawer>`,
  sheets, settings push, connection toast. Startup effect (load settings
  → connect CP → list projects).
- `shell/TopBar.tsx` — hamburger, session title, status dot.
- `shell/Drawer.tsx` — left slide-out: sessions list (grouped by
  project), "+ New chat" (opens workflow picker sheet), persona row,
  settings entry.
- `shell/BottomSheet.tsx` — generic sheet primitive (CSS transforms,
  drag-to-dismiss).
- `shell/PushScreen.tsx` — full-screen slide-in for settings.
- `chat/ChatSurface.tsx` — owns engine connection lifecycle for the
  active session. Wraps `<ChatView>` from `@lutin/chat-widgets` with
  mobile-tuned slots.
- `chat/Composer.tsx` — overrides chat-widgets `<Composer>` with mobile
  sizing; mic stub.
- `chat/useSession.ts` — opens engine WS, runs the chat reducer (port
  of `workflows/chat/ui/src/session.ts`), exposes `messages`,
  `sendUserText`, `turn`.
- `chat/ToolDetailSheet.tsx` — bottom sheet for a tapped tool-call
  message.
- `picker/WorkflowPickerSheet.tsx` — bottom sheet listing workflows
  from CP. v1 gates to `id === "chat"`.
- `settings/SettingsScreen.tsx` — push view: CP URL field, token
  field, "Test connection", save.
- `settings/storage.ts` — `localStorage` get/set for `{cpUrl, token}`.
- `state/store.ts` — zustand store: projects, sessions, conn, selected.
- `wire/cp.ts` — thin wrapper around `@lutin/lutin-ws-client`'s
  `cpClient` (`useCpRequest`, broadcast subscription hook).

## Shared code: `packages/lutin-ws-client/`

Lift wire layer fresh in TS (desktop currently does WS in Rust at
`lutin-desktop/src-tauri/{cp.rs,bridge.rs}`; mobile needs it in TS — we
don't refactor desktop now).

- `frame.ts` — postcard encode/decode of `Frame::Hello` /
  `Frame::Payload` / `Frame::Broadcast`. Reuse the postcard primitives
  from `workflows/chat/ui/src/postcard.ts`.
- `cpClient.ts` — browser `WebSocket` wrapper; Hello handshake,
  request/response correlation by `request_id`, broadcast emitter.
- `engineClient.ts` — same shape but for per-session workflow engine WS
  (`WorkflowSession` scope token).
- `types.ts` — port the CP-relevant subset of `lutin-desktop/src/types.ts`
  (`CpEvent`, `Request`, `Response`, `ProjectInfo`, `SessionInfo`,
  `WorkflowInfo`). Drop desktop-only types (Tts*, Keybind*, PluginOpened)
  for v1.

`workflows/chat/ui/src/{chat.ts,session.ts,adapter.ts,postcard.ts}` are
pure logic. Copy into `lutin-mobile/src/chat/` for v1; promote to
`packages/chat-session/` only when a second consumer wants them.

## Auth/pairing (v1)

Paste-token flow: user runs the existing CLI on the host to mint a
control-panel-scoped token, pastes it + `wss://host:port` into the
mobile Settings screen, persisted to `localStorage`. Justification:
zero backend work, exercises the same `lutin_auth::verify` path the
desktop uses, ships this week.

v2: CP serves a one-time pairing code on a small HTTP endpoint; mobile
scans QR encoding `{url, code}`; CP swaps code for token.

## Backend changes — ideally zero

- **CORS:** WebSocket handshake is exempt from preflight; CP doesn't
  validate `Origin` (`tokio_tungstenite::accept_async`). Browsers
  connect fine.
- **TLS:** PWA install requires HTTPS, which forces `wss://`. Document
  Caddy/nginx termination in front of CP. Dev: serve mobile over
  `http://` on LAN.
- **Postcard in browser:** existing `workflows/chat/ui/src/postcard.ts`
  is self-contained, sufficient.

## Risks & mitigations

1. **Postcard codec drift between Rust and TS** — *mitigation:* reuse
   the existing TS postcard from `workflows/chat/ui`; add round-trip
   tests against canonical Rust fixtures.
2. **TLS for production PWA** — *mitigation:* document reverse-proxy
   setup; defer to v2.
3. **Token in `localStorage` (XSS = full compromise)** — *mitigation:*
   strict CSP, no inline scripts, no third-party CDN. v2 considers
   Web Crypto + passphrase wrap.
4. **Reconnect/resync on flaky mobile networks** — broadcast lag
   triggers server-side disconnect (`Lagged(n)` in CP `lib.rs:858`).
   *Mitigation:* on reconnect, re-issue `ListProjects` + `ListSessions`
   and re-subscribe to active engine session; chat reducer is replay-safe.
5. **Workflow UI fragmentation** — chat renders directly; other
   workflows ship iframe UIs. *Mitigation:* filter
   `WorkflowInfo.id === "chat"` in v1's picker; iframe hosting in v2.

## Status

- [x] **Step 0 — Scaffold.** `lutin-mobile/` package boots, builds clean
      (193 kB JS / 22 kB CSS), `@lutin/chat-widgets/theme.css` imports,
      placeholder renders. Vite dev server on port 1430 with `host: true`
      for LAN access.
- [ ] **Step 1 — `packages/lutin-ws-client/`.** Frame codec + browser WS
      wrapper + handshake. Next.
- [ ] Step 2 — Settings push view + paste-token storage.
- [ ] Step 3 — CP connect, list sessions in drawer.
- [ ] Step 4 — Open session, render chat, send/receive one message.
- [ ] Step 5 — Mobile shell polish (top bar, drawer, sheets).
- [ ] Step 6 — Composer (mic stub).
