# STT settings UI plan

Self-contained brief for the next session. Background: `lutin-stt`
already wires both whisper.cpp and NVIDIA Parakeet TDT behind a
single `SttWorker` trait, CP downloads + caches the right model on
first use, the wire surface is `SttConfig::{Whisper, Parakeet}`, and
the desktop persists the selection in `~/.config/lutin/desktop.json`
under `stt`. There is no UI for it yet — switching backends today
requires a hand-edit. This slice adds that UI.

## Goal

Let the user pick STT backend + per-backend params from the Settings
view. Save round-trips through the existing `settingsGet` /
`settingsSet` pair; first PTT after a save uses the new selection
(no app restart). Model download still happens lazily on first use
in CP.

## Files to touch

| File | What |
|---|---|
| `lutin-desktop/src/components/SettingsView.tsx` | Add `Tab = "stt"`, sidebar entry, `<StTPanel>` component (~120 LOC, mirror `WebSearchPanel`) |
| `lutin-desktop/src/components/SettingsView.module.css` | Tiny additions if any new layout primitive is needed; reuse existing `formRow` / `field` |
| (no Rust changes) | `settings_get` / `settings_set` already pass `DesktopSettings` whole; the `stt` field is already round-tripped |

## UI shape

Top-level radio-style picker, then the matching backend panel below.
Externally-tagged enum makes draft state mechanical:
`draft.backend === "Whisper" ? draft.whisper : draft.parakeet`.

```
┌─ Speech-to-text ─────────────────────────────────┐
│ Backend                                          │
│  ◉ Whisper (CPU/GPU, multilingual, accurate)     │
│  ○ Parakeet (NVIDIA, ~10× faster, 25 langs)      │
│                                                  │
│ ── Whisper settings ──────────────────────────── │
│ Model     [▾ large-v3-turbo                    ] │
│            (or distil-large-v3 — smaller/faster) │
│ Language  [    ] (blank = auto-detect)           │
│ Beam size [— ●——————]  1 (greedy) … 8            │
│                                                  │
│        [ Discard ]   [ Save ]                    │
└──────────────────────────────────────────────────┘
```

When "Parakeet" is selected the second section swaps to:

```
│ ── Parakeet settings ────────────────────────── │
│ Model     [▾ tdt-0.6b-v3                      ] │
│ ⓘ First use downloads ~2.6 GB of ONNX weights  │
│   into <config_dir>/models/parakeet/<model>/.  │
```

## State + draft pattern

Mirror `WebSearchPanel` (SettingsView.tsx:550 onwards):

```tsx
type SttDraft = SttConfig;          // re-use the wire shape directly

const [draft, setDraft] = useState<SttDraft | null>(null);
const [initial, setInitial] = useState<SttDraft | null>(null);

useEffect(() => {
  settingsGet().then(s => { setDraft(s.stt); setInitial(s.stt); });
}, []);

const dirty = JSON.stringify(draft) !== JSON.stringify(initial);

async function save() {
  const all = await settingsGet();
  await settingsSet({ ...all, stt: draft! });
  setInitial(draft);
}
```

Notes:
- Switching the backend radio swaps the draft variant entirely; the
  unselected branch's prefs aren't preserved across the toggle (the
  externally-tagged enum has no place to keep them). If we want
  sticky per-backend prefs, hold a separate `lastWhisperPrefs` /
  `lastParakeetPrefs` ref and re-hydrate on toggle. Probably fine to
  skip in v1 — there's only one Parakeet model anyway.
- All controls are uncontrolled-from-user-perspective drafts; the
  wire write happens only on Save. Discard reverts to `initial`.
- `language` is a free-text input today — three-letter ISO is too
  loose for a dropdown and `whisper-rs` accepts the full set. A
  blank string should serialise as `null` (or omitted) so the wire
  matches `WhisperConfig.language: Option<String>`.

## Tauri / wire details

- `SttConfig` is externally tagged. The TS type in `types.ts` is
  `{ Whisper: WhisperConfig } | { Parakeet: ParakeetConfig }` — match
  on the variant key, never assume a `kind` discriminator.
- `settings_set` is whole-object replace. Read first, splice `stt`,
  write back.
- No new CP request needed — backend swap takes effect on the next
  `OpenTranscription`, which already carries the full `SttConfig`
  per stream.

## Polish to consider (cut if it adds days)

- **Model warmup on save.** After Save, fire one
  `OpenTranscription` + `CancelTranscription` round-trip so the
  first real PTT doesn't pay the load cost. CP already runs warmup
  on `OpenTranscription`; we'd just be triggering it preemptively.
- **Download progress.** Parakeet's first run pulls ~2.6 GB silently.
  CP's `download_to_with_progress` helper already exists (TTS uses
  it). Wire it through a new `Event::SttModelProgress` and show a
  `TtsDownloadToast`-style banner. If skipping, at minimum show a
  spinner overlay while the first PTT after a backend swap is
  pending — the current "nothing happens for 2 minutes" UX is bad.
- **Capability hints.** Show a subtle "no language hint" / "ignores
  beam size" note next to disabled fields when Parakeet is active,
  so users understand why the controls vanish.

## Out of scope for this slice

- Streaming partials UI (Parakeet TDT one-shot today; needs `EOU`
  variant + new wire event + `transcribe_stream` method on the
  trait — separate slice).
- Custom-model upload (`max_model_upload_bytes` in `LimitsSettings`
  hints at a future feature; no endpoint exists).
- Per-project STT override. `lutin-settings::Settings` doesn't have
  an STT field today (we deleted the dead `SttSettings`). If
  per-project override is wanted, re-introduce it as a typed enum
  there + thread through CP — separate slice.

## Acceptance check

1. Open Settings → Speech-to-text. Defaults reflect what's in
   `desktop.json` (Whisper/large-v3-turbo on a fresh install).
2. Switch to Parakeet → Save. Restart the app. Settings still shows
   Parakeet.
3. Press PTT. First press downloads weights (slow) and transcribes;
   second press is fast.
4. Switch back to Whisper → Save. Next PTT uses cached whisper
   worker (no re-download).
5. `~/.config/lutin/desktop.json` contains
   `"stt": { "Parakeet": { "model": "tdt-0.6b-v3" } }` after step 2
   and `"stt": { "Whisper": { … } }` after step 4.

## Quick references

- Wire: `crates/lutin-control-protocol/src/lib.rs` — search
  `SttConfig`, `WhisperConfig`, `ParakeetConfig`, `WhisperModel`,
  `ParakeetModel`.
- Desktop persistence: `lutin-desktop/src-tauri/src/settings.rs:91`
  (`pub stt: SttConfig`).
- Tauri commands: `lutin-desktop/src-tauri/src/lib.rs:653`
  (`settings_get`), `:658` (`settings_set`).
- TS types: `lutin-desktop/src/types.ts` — `SttConfig`,
  `WhisperConfig`, `ParakeetConfig`, `WhisperModel`, `ParakeetModel`.
- Closest existing pattern: `lutin-desktop/src/components/SettingsView.tsx:550-650`
  (`WebSearchPanel`).
- CP-side download paths (already implemented, no UI hookup yet):
  `lutin-control-panel/src/transcribe.rs` — `ensure_whisper_model`,
  `ensure_parakeet_model`.
