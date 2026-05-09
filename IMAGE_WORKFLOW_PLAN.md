# Image Workflow Plan

A new Lutin workflow for local image generation. Mirrors the chat workflow's shape: a chat-style UI where the user types a prompt and generated images appear inline in the scrollback.

## Backend: ComfyUI (external)

ComfyUI is a hard prerequisite. The user installs and runs it themselves — Lutin does not bundle, install, or manage the ComfyUI process.

- The workflow connects to a configurable URL (default `http://127.0.0.1:8188`).
- On first load (or when generation fails), the workflow shows a clear "ComfyUI not reachable at <url>" empty state with a link to install instructions and a settings entry to change the URL.
- No process management on Lutin's side.

## Architecture

HTTP/WebSocket talk to ComfyUI lives in **Rust (engine-side)**, not in the workflow iframe. Reasons:

- Matches the existing chat pattern (workflow → `lutin.request` → engine → external service → response).
- Workflow stays a sandboxed iframe with no network capability needed.
- Image saving needs filesystem access, which is engine-side.
- Streaming progress fits the existing broadcast channel.

```
workflows/image/
  Cargo.toml
  Dockerfile
  manifest.toml
  src/                     # Rust backend, built on lutin-workflow-sdk
    main.rs                # entry; ComfyUI client + image storage
    comfy.rs               # HTTP/WS client
    templates.rs           # template loading + node patching
  templates/
    flux-schnell.json      # parameterizable graph template (default)
  ui/
    package.json
    public/lutin.workflow.json
    src/
      App.tsx              # chat-like layout, image rendering
      adapter.ts
      session.ts           # reducer: prompts, in-flight jobs, gallery
      lutin.ts
packages/image-protocol/   # encode/decode Request | Response (shared by ui + backend)
```

## Manifest capabilities

- `receive_transcription` — so the user can dictate prompts via PTT/open-mic.
- No `tts`, no `sub_agents`.

## Protocol (`@lutin/image-protocol`)

**Requests** (workflow → engine via `lutin.request`):
- `generate { prompt, negative_prompt?, template_id, count, steps, cfg, seed?, width, height }`
- `cancel { job_id }` — best-effort. If the job is currently running, calls ComfyUI `/interrupt`. If it's still queued, calls `DELETE /queue` to dequeue. The UI must treat cancel as best-effort: a partially-complete image may still arrive.
- `listTemplates` — returns the graph templates Lutin ships (user-facing "models"). Each template references a checkpoint by filename (e.g. `flux1-schnell.safetensors`); the user is responsible for installing checkpoints through ComfyUI itself. The workflow does not download or manage model files.
- `listSessions` / `loadSession` / `newSession` — same pattern as chat
- `getSettings` / `setSettings` — at minimum `comfyui_url`, default template, default size

**Responses + broadcasts** (engine → workflow):
- `jobQueued { job_id }`
- `jobProgress { job_id, step, total_steps }` (broadcast, bridged from ComfyUI WS)
- `jobImage { job_id, index, image_id, mime, bytes_b64, thumbnail_b64 }` (one per image as it lands; image bytes carried inline as base64 so the iframe can render via `data:` URL with no filesystem access)
- `jobDone { job_id }`
- `jobError { job_id, message }` — including "ComfyUI unreachable"

**Image delivery / iframe rendering.** Workflows have no filesystem access. Image bytes flow through the protocol:
- New generations carry base64-encoded PNG + thumbnail in the `jobImage` event. The iframe renders via `data:image/png;base64,...` URLs (or `URL.createObjectURL(new Blob(...))` for large ones).
- The Rust crate also writes the image to disk for persistence (see Storage). The on-disk copy is the source of truth; the base64 in the event is a one-shot for live display.
- For session restore, eagerly base64-encoding every historical image would balloon `loadSession` responses. Instead, `loadSession` returns transcript metadata only (prompts + `image_id` references), and the workflow calls `getImage { image_id }` lazily per image as it scrolls into view. `getImage` returns the same `{ mime, bytes_b64, thumbnail_b64 }` shape.

## Rust backend (`workflows/image/src/`)

- HTTP client to the configured ComfyUI URL.
- Generates a stable `client_id` (UUID) once per process. The same `client_id` is included on every `/prompt` POST and on the `/ws?clientId=<id>` connection — ComfyUI only delivers progress for prompts whose `client_id` matches the WS connection.
- Persistent WebSocket listener on `/ws` for progress events; reconnects with backoff on drop. Events are bridged to Lutin's broadcast channel, keyed by `prompt_id` so the workflow can match them to in-flight jobs.
- Loads a graph template JSON from disk, patches the prompt/seed/steps/cfg/size nodes by ID, POSTs to `/prompt`.
- On the WS `executed`/`execution_success` event for a `prompt_id`, fetches `/history/{prompt_id}` to discover output filenames, then downloads each via `/view`.
- Writes images to the per-session images dir (see Storage), generates a small thumbnail (webp).
- Settings file: `lutin.image.toml` with `comfyui_url`, default template, default size, default count.

### Errors to surface

- **ComfyUI unreachable** (`/system_stats` health check fails) — empty-state with the configured URL and a settings shortcut.
- **Missing checkpoint** — ComfyUI rejects the prompt because the template's referenced checkpoint isn't installed. Surfaced as `jobError` with the missing filename and a hint to install it through ComfyUI.
- **Workflow execution error** — ComfyUI accepts the POST (200) but execution fails inside the graph. The error appears in `/history/{prompt_id}` under `status.messages`, not in the POST response. The crate must check this and emit `jobError`.
- **OOM / sampler failure** — surfaces the same way (in `/history`); pass the message through verbatim.
- **WS disconnect mid-job** — keep polling `/history` as a fallback so a job in flight isn't lost when the socket flaps.

## UI

- Chat-style scrollback. Each "turn" = one prompt + the resulting image grid (1–N images).
- Composer: prompt textarea, model dropdown, "advanced" disclosure for steps/cfg/size/seed/count.
- Per-image actions: open in viewer, copy path, regenerate with same seed, "use as starting point" (placeholder for future img2img).
- Live progress bar per in-flight job (from broadcasts).
- Model concept: ship a small set of graph templates as the user-facing "models" (start with FLUX schnell; add SDXL base if/when needed). The dropdown selects a template; the advanced panel reflects what that template accepts.

## Storage layout

Project-scoped, same pattern as chat (`<project>/.lutin/<workflow>/...`):

```
<project>/.lutin/image/
  sessions.toml              # list of sessions (mirror chat)
  <session_id>/
    summary.json
    transcript.json          # prompts + image refs (paths relative to this session dir)
    images/
      <ts>-<seed>-0.png
      <ts>-<seed>-0.thumb.webp
```

The `image_id` carried in protocol events is the relative path (e.g., `<session_id>/images/<ts>-<seed>-0.png`); `getImage` resolves it against the project's `.lutin/image/` root.

## Phasing

1. **Slice 1 — end-to-end skeleton.** Rust crate stub that POSTs a hardcoded FLUX-schnell graph with a hardcoded prompt and saves the result. Verify ComfyUI integration outside the workflow.
2. **Slice 2 — workflow shell.** New `workflows/image/`, manifest, render a static "hello" page, wire it into the sidebar.
3. **Slice 3 — generate path.** Protocol package, `lutin.request` wired to `lutin-image::generate`, single-image generation, render in chat scrollback.
4. **Slice 4 — progress streaming.** WebSocket bridge, broadcast events, progress UI.
5. **Slice 5 — settings & multi-image.** ComfyUI URL setting, model dropdown, count, steps/cfg/seed/size, gallery layout.
6. **Slice 6 — sessions.** Save/load, mirroring chat's session pattern.
7. **Later.** img2img, ControlNet, LoRAs, upscaling — each is a new graph template + a few extra params.

## Settings UI

Inlined in the image workflow itself, mirroring how chat exposes persona/TTS controls in its own UI rather than via a global Lutin settings panel. The composer has an "advanced" disclosure for per-generation params (steps/cfg/size/seed/count); a separate settings affordance (gear icon or panel) holds workflow-level config (`comfyui_url`, default template, default size, default count).

## Default model

FLUX schnell. Ship a single template (`templates/flux-schnell.json`) referencing `flux1-schnell.safetensors`. Adding more templates later is just dropping JSON files into `templates/` — no protocol changes.
