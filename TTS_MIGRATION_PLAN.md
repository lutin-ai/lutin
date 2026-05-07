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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrpheusModel {
    /// `orpheus-3b-0.1-ft-Q4_K_M.gguf` — current default. Add new
    /// variants for new GGUF exports; the closed enum prevents the
    /// wire from pivoting to arbitrary filenames/URLs (same pattern
    /// as `WhisperModel`).
    ThreeBQ4_K_M,
}

/// Documented voices for the Orpheus 3B 0.1-ft model. Closed enum so
/// a workflow can't pass arbitrary strings into the prompt template.
/// If we add a new model with a different voice set, that becomes a
/// new `OrpheusVoice` variant or — if voice sets diverge enough — a
/// new outer `TtsBackend` variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrpheusVoice {
    Tara,
    Leah,
    Jess,
    Leo,
    Dan,
    Mia,
    Zac,
    Zoe,
}
```

`OpenTtsStream` carries `backend: TtsBackend` directly — no
`TtsConfig` wrapper. The wrapper would have exactly one field today;
add it later if cross-backend options (e.g. `output_sample_rate`,
`pre_buffer_ms`) actually materialise.

**New `Request` variants:**

```rust
/// Pre-download / pre-load weights for a backend without opening a
/// stream. Returns once the GGUF + SNAC (or backend-equivalent) are
/// on disk and the factory has loaded into VRAM. Mirrors the
/// `whisper_ensure_model` pattern — workflows / settings UI call
/// this from the user's "enable TTS" toggle so the first
/// `OpenTtsStream` doesn't block for minutes on a fresh install.
EnsureTtsBackend { backend: TtsBackend },

OpenTtsStream { backend: TtsBackend },
SpeakTts { stream_id: TtsStreamId, text: String, speed: f32 },
CancelTts { stream_id: TtsStreamId },
CloseTtsStream { stream_id: TtsStreamId },
```

`OpenTtsStream` assumes the backend's weights are loaded. If they
aren't, it returns `ApiError::TtsBackendNotReady` rather than
silently downloading — that contract keeps the open call fast and
predictable, and gives the UI a clean place to show progress for
`EnsureTtsBackend` instead.

**New `ResponseOk` variant** (only `OpenTtsStream` returns a
payload — the rest are `Ack`):

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
ApiError::TtsBackendNotReady   // OpenTtsStream before EnsureTtsBackend
ApiError::TtsLimit(TtsLimit)

pub enum TtsLimit {
    TooManyStreams { max: usize },   // process-wide
    TextTooLong { got: usize, max: usize },  // enforced at SpeakTts
}
```

Add roundtrip tests in the protocol's `mod tests` (mirror
`open_transcription_roundtrip`).

**Voice/model typing — locked in.** Use closed enums on the wire
(`OrpheusModel`, `OrpheusVoice`) rather than free-form strings. This
matches `WhisperModel` and gives us the same "wire surface can't
pivot to arbitrary files" guarantee. Map enum → backend-internal
string (`"tara"`, `"leah"`, …) inside CP at the boundary into
`lutin-tts`.

**Single id space.** `TtsStreamId(u32)` is the *only* id; CP passes
`lutin_tts::StreamId(wire_id.0 as u64)` into the service. No
internal/external mapping table to keep in sync.

## Slice 3 — CP-side wiring

**Files:** `lutin-control-panel/src/tts.rs`,
`lutin-control-panel/src/tts_streams.rs`,
edits to `lib.rs` for dispatch + the broadcast pump.

**Hoist `download_streaming` first.** Move
`lutin-control-panel/src/transcribe.rs::download_streaming` (plus
the temp-rename scheme) into a new
`lutin-control-panel/src/downloads.rs`, exporting one
`download_to(url, dest) -> Result<()>` helper. Update
`transcribe.rs` to use it. This is a separate prepatory commit
before slice 3 proper — three copies of the same downloader (legacy
whisper, current `transcribe.rs`, and our new `tts.rs`) is one too
many.

**`tts.rs` — model fetch + factory cache.** Mirrors
`transcribe.rs`:

- `models_dir(global_config_dir)` → `<config>/models/orpheus/`.
- `ensure_orpheus_gguf(global_config_dir, OrpheusModel)` and
  `ensure_snac_onnx(global_config_dir)` use the hoisted
  `download_to` helper.
- URLs: copy from `../lutin/engine/src/tts/model.rs`:
  - Orpheus 3B Q4_K_M:
    `https://huggingface.co/isaiahbjork/orpheus-3b-0.1-ft-Q4_K_M-GGUF/resolve/main/orpheus-3b-0.1-ft-Q4_K_M.gguf`
  - SNAC decoder:
    `https://huggingface.co/onnx-community/snac_24khz-ONNX/resolve/main/onnx/decoder_model.onnx`
- A `TtsBackends` registry holding lazily-loaded `TtsService`s keyed
  on a model-identity discriminant (voice doesn't affect model
  load, so two streams with different voices share the same
  service):

  ```rust
  /// Cache key for a loaded backend. Each variant captures only the
  /// fields that determine which weights are in VRAM — voice and
  /// other per-utterance config are excluded. As more backends land,
  /// add variants here, never reuse one.
  #[derive(Clone, Copy, Hash, PartialEq, Eq)]
  enum BackendKey {
      Orpheus(OrpheusModel),
      // Kokoro(KokoroModel),  // future
  }

  fn backend_key(b: &TtsBackend) -> BackendKey {
      match b {
          TtsBackend::Orpheus { model, .. } => BackendKey::Orpheus(*model),
      }
  }

  pub struct TtsBackends {
      services: Mutex<HashMap<BackendKey, Arc<TtsService>>>,
      sink_tx: mpsc::UnboundedSender<TtsEvent>,
      config_dir: PathBuf,
  }
  ```

  `EnsureTtsBackend { backend }` runs the model fetch +
  `OrpheusFactory::load(...)` on `spawn_blocking`, then
  `TtsService::new(Box::new(factory), sink, DEFAULT_WORKER_COUNT)`,
  inserts under `backend_key(&backend)`. `OpenTtsStream` looks up
  the key and returns `TtsBackendNotReady` on miss instead of
  loading.

**`tts_streams.rs` — registry.** Mirrors
`transcription_streams.rs`:

```rust
pub struct Stream {
    pub id: TtsStreamId,
    pub backend: TtsBackend,
    pub service: Arc<TtsService>,   // points at the loaded backend
}
```

- `MAX_OPEN_STREAMS` cap (32, matching transcription).
- `open(backend, service) -> Result<TtsStreamId, TtsLimit>`
  allocates the wire id; the service consumes the same value as
  `lutin_tts::StreamId(id.0 as u64)` so there's no second id space
  to track.
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
      broadcast(Event::TtsAudio {
          stream_id: TtsStreamId(stream_id.0 as u32),
          chunk,
      })
  ```
  Single id space — see slice 2.

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
  drains the queue belonging to the currently active session;
  others are held untouched.
- The CP event listener (in `dispatch.rs` or
  `lib.rs::run_app`) receives `Event::TtsAudio` / `TtsFinished` and
  pushes bytes into the right queue. Drop chunks for streams the
  desktop doesn't know about (defensive — broadcast can deliver
  events for streams owned by other clients in multi-desktop
  setups, though we ship single-client today).
- **Active-stream selection.** Each TTS stream is bound to a
  session at `tts_open_stream` time (the calling workflow's active
  session id). Reuse the existing `set_active_session` from Phase
  3a: when the active session changes, the playback module
  (a) immediately silences output, (b) drops queued PCM for streams
  bound to the previously-active session — held audio after a
  context switch is worse than losing it. New audio for those
  streams keeps arriving over the wire and gets dropped on
  enqueue with a single rate-limited warn.
- **Cancel cascade.** `tts_cancel(stream_id)`:
  1. calls CP `CancelTts` (in-flight synthesis stops, queued
     sentences drop on the CP side);
  2. before awaiting the response, drains the desktop-side queue
     for that stream synchronously so any already-broadcast
     PCM that hasn't been played yet is discarded.
  Without step 2 the user hears tail audio after pressing stop.

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

**Capability gate.** Enforced in `lutin-desktop/src-tauri/src/dispatch.rs`
at the same layer that gates `receive_transcription`. `tts_open_stream`
(and consequently `tts_speak` / `tts_cancel` / `tts_close_stream` for
stream ids that weren't opened by the calling workflow) reject
unless the workflow's manifest declares `"tts"` in `capabilities`.
`EnsureTtsBackend` is also gated — without the capability you can't
even pre-download.

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
