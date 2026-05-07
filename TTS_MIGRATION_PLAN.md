# TTS migration plan

Port Orpheus TTS from `../lutin/engine/src/tts/` into lutin-new with a
pluggable backend trait so we can swap Orpheus for Kokoro / a cloud
API later without churning callers.

## Status (as of 2026-05-07)

- **Slice 1 — `crates/lutin-tts` crate. DONE.** Commits `e40f463` +
  review-fix `1d17c87`.
- **Slice 2 — protocol additions.** Pending.
- **Slice 3 — CP-side wiring.** Pending.
- **Slice 4 — desktop playback.** Pending.
- **Slice 5 — workflow shim + capability.** Pending.

## Locked-in design choices

These were resolved at the start of the work — don't relitigate
without a reason.

1. **TTS lives in CP, not desktop.** Same reason as Whisper: GPU
   weights, big binaries, desktop stays thin. Desktop receives PCM
   bytes and plays them.
2. **Workflow-driven `speak()` calls.** Workflows decide what to say
   (e.g. chat splits the LLM stream into sentences and calls
   `lutin.tts.speak(sentence)` per sentence). No engine-side
   sentence-aggregator hook coupling TTS to chat semantics.
3. **Per-stream backend config.** `OpenTtsStream` carries which
   backend the workflow wants. CP lazy-loads each backend on first
   use and keeps it loaded. (Single-backend factory match is a
   different choice we ruled out for flexibility.)
4. **Orpheus only for v1.** Kokoro / qwen3 ports are not in scope.
   The trait is shaped to accept them later.
5. **Capability-gated.** Workflow manifest must declare
   `capabilities: ["tts"]` to open a stream. Mirrors the
   `receive_transcription` pattern.

## Slice 1 (DONE) — what landed

- New `crates/lutin-tts` crate.
- `TtsBackendFactory` + `TtsWorker` traits (`backend.rs`).
- `OrpheusFactory` + `OrpheusWorker` ported from legacy engine
  (`orpheus/mod.rs`); `SnacDecoder` ported (`orpheus/snac.rs`).
- `TtsService` (`service.rs`): backend-agnostic worker pool with
  ordered per-stream delivery. Stream-scoped (`StreamId(u64)`) — the
  legacy `chat_id`+`connection_id` routing pair is gone. Output is a
  `tokio::mpsc::UnboundedSender<TtsEvent>` sink.
- `TtsEvent { Audio { stream_id, chunk: Vec<u8> }, Finished { stream_id } }`.
- Real worker pool via `crossbeam-channel` (the legacy
  `Arc<Mutex<Receiver>>` pattern that serialised all workers on one
  lock has been excised).
- `is_valid_voice` validates voice names at the `speak` boundary
  before they hit the model prompt template — `[A-Za-z0-9_-]{1..=64}`,
  rejects anything that could break the prompt or smuggle in tokens.
- `clean_for_speech` strips markdown so the model doesn't pronounce
  formatting characters literally.

## Slice 2 — protocol additions

Mirrors the transcription-stream shape that landed in `c4d49b9`.

**Add to `crates/lutin-control-protocol/src/lib.rs`:**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TtsStreamId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TtsBackend {
    Orpheus { model: OrpheusModel, voice: OrpheusVoice },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrpheusModel {
    /// `orpheus-3b-0.1-ft-Q4_K_M.gguf` — current default. Add new
    /// variants for new GGUF exports; the closed enum prevents the
    /// wire from pivoting to arbitrary filenames/URLs (same pattern
    /// as `WhisperModel`).
    ThreeBQ4_K_M,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrpheusVoice {
    /// Default voice. Add variants per documented Orpheus voice
    /// (tara, leah, leo, jess, …). Closed enum so a workflow can't
    /// pass arbitrary strings into the prompt template.
    Tara,
    Leah,
    // …
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TtsConfig {
    pub backend: TtsBackend,
}
```

**New `Request` variants:**

```rust
OpenTtsStream { config: TtsConfig },
SpeakTts { stream_id: TtsStreamId, text: String, speed: f32 },
CancelTts { stream_id: TtsStreamId },
CloseTtsStream { stream_id: TtsStreamId },
```

**New `ResponseOk` variant** (only `OpenTtsStream` returns a payload —
the rest are `Ack`):

```rust
TtsStreamOpened { stream_id: TtsStreamId },
```

**New `Event` variants** (push channel, broadcast — clients filter by
the stream id they own):

```rust
TtsAudio { stream_id: TtsStreamId, chunk: Vec<u8> },
TtsFinished { stream_id: TtsStreamId },
```

**New `ApiError` + limit variants:**

```rust
ApiError::TtsStreamNotFound(TtsStreamId)
ApiError::TtsLimit(TtsLimit)

pub enum TtsLimit {
    TooManyStreams { max: usize },   // process-wide
    TextTooLong { got: usize, max: usize },
}
```

Add roundtrip tests in the protocol's `mod tests` (mirror
`open_transcription_roundtrip`).

**Voice/model typing — locked in.** Use closed enums on the wire
(`OrpheusModel`, `OrpheusVoice`) rather than free-form strings. This
matches `WhisperModel` and gives us the same "wire surface can't
pivot to arbitrary files" guarantee. Map enum → backend-internal
string (`"tara"`, `"leah"`) inside CP at the boundary into
`lutin-tts`.

## Slice 3 — CP-side wiring

**Files:** `lutin-control-panel/src/tts.rs`,
`lutin-control-panel/src/tts_streams.rs`,
edits to `lib.rs` for dispatch + the broadcast pump.

**`tts.rs` — model fetch + factory cache.** Mirrors
`transcribe.rs`:

- `models_dir(global_config_dir)` → `<config>/models/orpheus/`.
- `ensure_orpheus_gguf(global_config_dir, OrpheusModel)` and
  `ensure_snac_onnx(global_config_dir)` — atomic-rename downloads
  using the same `download_streaming` helper (consider hoisting it
  out of `transcribe.rs` into a shared module since this is now its
  third copy from the legacy engine).
- URLs: copy from `../lutin/engine/src/tts/model.rs`:
  - Orpheus 3B Q4_K_M:
    `https://huggingface.co/isaiahbjork/orpheus-3b-0.1-ft-Q4_K_M-GGUF/resolve/main/orpheus-3b-0.1-ft-Q4_K_M.gguf`
  - SNAC decoder:
    `https://huggingface.co/onnx-community/snac_24khz-ONNX/resolve/main/onnx/decoder_model.onnx`
- A `TtsBackends` registry holding lazily-loaded `TtsService`s keyed
  on a hash of the backend config (or per-`OrpheusModel`, since
  voice doesn't affect model load):

  ```rust
  pub struct TtsBackends {
      orpheus: Mutex<HashMap<OrpheusModel, Arc<TtsService>>>,
      sink_tx: mpsc::UnboundedSender<TtsEvent>,
      config_dir: PathBuf,
  }
  ```

  First `OpenTtsStream` for a given `OrpheusModel` runs
  `ensure_orpheus_gguf` + `ensure_snac_onnx` on `spawn_blocking`,
  then `OrpheusFactory::load(...)` (also blocking), then
  `TtsService::new(Box::new(factory), sink, DEFAULT_WORKER_COUNT)`.
  Subsequent opens reuse the cached service.

**`tts_streams.rs` — registry.** Mirrors
`transcription_streams.rs`:

```rust
pub struct Stream {
    pub id: TtsStreamId,
    pub config: TtsConfig,
    pub service: Arc<TtsService>,   // points at the loaded backend
    pub internal_id: lutin_tts::StreamId,  // monotonic per-CP
}
```

- `MAX_OPEN_STREAMS` cap (32, matching transcription).
- `open(config) -> Result<TtsStreamId, TtsLimit>` allocates
  `TtsStreamId` (next_id) and maps to a fresh
  `lutin_tts::StreamId(next_internal)` for the service.
- `find(id) -> Option<&Stream>` for `Speak` / `Cancel`.
- `take(id)` for `Close`.

**`lib.rs` dispatch:**

- New match arms for the four request variants. Each looks up the
  stream, calls into the service, returns `ResponseOk::Ack` (or
  `TtsStreamOpened` for `OpenTtsStream`).
- `SpeakTts`: enforce a `MAX_TEXT_LEN` (e.g. 4096 chars — the model
  has a 2048-token context, so longer inputs are silently truncated
  by the worker; reject them at the boundary instead).
- The sink mpsc — there's exactly one process-wide receiver. CP
  spawns a task that pumps every `TtsEvent` into a broadcast frame:
  ```rust
  TtsEvent::Audio { stream_id, chunk } =>
      broadcast(Event::TtsAudio { stream_id: wire_id(stream_id), chunk })
  ```
  The `internal → wire` id map lives in `TtsBackends` (or a separate
  small map; the mapping is allocated at `open` time).

## Slice 4 — desktop playback

**New file:** `lutin-desktop/src-tauri/src/tts_playback.rs`.

- `cpal` output stream. Default output device, 24 kHz mono i16 (the
  Orpheus contract). If the device's native rate isn't 24 kHz, use a
  resampler (`rubato` is already in our deps via whisper's chain —
  check; otherwise add it). Old desktop did this in
  `desktop-old-design/src/audio/playback.rs` — reference but don't
  port wholesale, that code carries egui + the old protocol.
- A per-stream PCM queue: `HashMap<TtsStreamId, VecDeque<i16>>`
  guarded by a `Mutex` on the cpal callback side. Output callback
  drains the active stream's queue.
- The CP event listener (in `dispatch.rs` or
  `lib.rs::run_app`) receives `Event::TtsAudio` / `TtsFinished` and
  pushes bytes into the right queue. Drop chunks for streams the
  desktop doesn't know about (defensive — broadcast can deliver
  events for streams owned by other clients in multi-desktop
  setups, though we ship single-client today).
- Active-stream selection: the desktop tracks "which stream id
  belongs to which workflow iframe" via the same active-session
  tracking that transcription uses. Audio for inactive workflows is
  buffered (or dropped — pick at slice time; recommend dropping with
  a warn, since holding back audio after the user has switched
  contexts is worse than losing it).

**Tauri commands** (in `dispatch.rs`):

- `tts_open_stream(config: TtsConfig) -> TtsStreamId` — calls CP
  `OpenTtsStream`, registers the stream with the playback module.
- `tts_speak(stream_id, text, speed)` → CP `SpeakTts`.
- `tts_cancel(stream_id)` → CP `CancelTts`; tells playback to drop
  any buffered chunks for that stream.
- `tts_close_stream(stream_id)` → CP `CloseTtsStream`; drop the
  playback registration.

Note the same `Vec<u8>`/`Vec<i16>` IPC quirk that `api.ts` already
handles: Tauri serialises bytes as a JSON number array. The PCM
direction is CP → desktop (Rust-only), so this affects only the
desktop → JS edge if we expose any audio bytes to JS (we shouldn't —
playback stays in Rust).

## Slice 5 — workflow shim + capability

**Capability gate.** `dispatch.rs` (or wherever the
transcription capability check sits) must reject `tts_open_stream`
unless the calling workflow's manifest includes
`"tts"` in `capabilities`. Same enforcement shape as
`receive_transcription`.

**Shim API** (lutin-desktop frontend, the chrome injects this into
the workflow iframe):

```ts
lutin.tts = {
  openStream(config: TtsConfig): Promise<TtsStreamId>,
  speak(streamId: TtsStreamId, text: string, opts?: { speed?: number }): Promise<void>,
  cancel(streamId: TtsStreamId): Promise<void>,
  closeStream(streamId: TtsStreamId): Promise<void>,
};
```

Each call invokes the matching Tauri command. The shim lives in the
same place the transcription shim lives (look for
`onTranscription` in the chrome's React side — the location of the
shim was added in Phase 3a slice 3 of the Tauri migration).

**Chat workflow integration (optional within slice 5).** Wire chat
to actually use TTS as a smoke test:

- Open a stream when the user enables TTS.
- Pipe assistant message text into a sentence aggregator (split on
  `. ! ? \n` after a minimum-length threshold — see legacy
  `engine/src/handler/chat.rs` for reference but don't port the
  engine-side hook; do it in the workflow's React code).
- Call `speak(streamId, sentence)` per sentence.
- Cancel on user interrupt / new message.

## Deferred review items (from slice 1 punch list)

These were flagged during the principle review but deferred. Pick up
when relevant:

- **`TtsError` + `SnacError` are stringly-typed.** Lose the
  underlying `llama_cpp_2::Error` / `ort::Error`. Worth doing if any
  downstream code wants to distinguish "model missing" from "GPU
  OOM". Not blocking.
- **`Mutex<HashMap>` for `cancel_tokens` → owner-task + command
  channel.** Cleaner per the message-passing principle, but the
  contention is one lock per `speak`/`cancel`, not on the audio
  hot path. Defer unless it bites.
- **Newtype `Voice`, `Speed`, `PcmChunk`, `AudioTokenRange`.** Best
  applied at the protocol boundary (slice 2 covers `OrpheusVoice` /
  `OrpheusModel`). `Speed` could move into the protocol as a parsed
  type if we want to enforce the 0.5–2.0 range there.
- **Pool-semantics tests via fake backend.** Useful: cover ordering
  per `StreamId`, `Finished` suppression between consecutive
  sentences for the same stream, cancel-then-respeak. Needs a fake
  `TtsBackendFactory` that emits deterministic chunks. Worth doing
  but not blocking.
- **Allocation churn on hot path** (`pcm.iter().flat_map(...).collect()`
  per chunk; `audio_tokens` not pre-sized). Real but micro;
  measure before optimising.

## Where to start when context is fresh

1. Read this file.
2. Look at the transcription stream as the reference pattern:
   - `crates/lutin-control-protocol/src/lib.rs` —
     `OpenTranscription` / `TranscribeChunk` / `FinishTranscription`
     / `CancelTranscription` request shapes (slice 2 mirrors this).
   - `lutin-control-panel/src/transcribe.rs` (model fetch shape).
   - `lutin-control-panel/src/transcription_streams.rs` (registry
     shape).
3. Look at the legacy TTS for the engine-side details that don't
   change: `../lutin/engine/src/tts/model.rs` (URLs),
   `../lutin/engine/src/tts/orpheus/mod.rs` (already ported), and
   `../lutin/engine/src/server.rs` line ~2027 for how the legacy
   engine wired model fetch + factory load (the order of operations
   we want to preserve).
4. Start with slice 2. Each slice ends in a runnable build + a
   commit; don't bundle.

## Things to remember (gotchas)

- **Tauri `Vec<u8>` IPC quirk.** `Vec<u8>` serialises as a JSON
  number array on the JS boundary. PCM bytes are CP → desktop
  (Rust-only), so this only matters if we expose audio to JS — we
  shouldn't.
- **`tauri::State` access from spawned tasks.** The TTS event pump
  needs the broadcast handle; the existing transcription path is
  the model.
- **Atomic-rename downloads must use the same temp-extension scheme
  as `transcribe.rs::download_streaming`** so corrupted partials
  can't be reused. Don't reinvent — hoist if you find yourself
  copying it.
- **Orpheus output is fixed at 24 kHz.** No in-model speed control;
  `_speed` is intentionally ignored in `orpheus/mod.rs`. If we ever
  want speed control, do it at the cpal output stage.
- **`OrpheusFactory::load` must run on `spawn_blocking`.** The
  legacy engine learned this the hard way — model load + Vulkan
  init both block the runtime.
