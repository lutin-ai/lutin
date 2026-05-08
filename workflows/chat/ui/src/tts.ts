// Chat-side TTS smoke test. Aggregates assistant deltas into
// sentences and pipes each into `lutin.tts.speak`. The chrome owns
// playback (`lutin-desktop/src-tauri/src/tts_playback.rs`) — this
// hook only deals in text.
//
// Sentence aggregation is deliberately simple: speak everything up
// through the last `.!?\n` followed by whitespace once the prefix is
// long enough. `e.g.` / `Mr.` etc. would split too eagerly with a
// pure punctuation rule, so we require trailing whitespace before
// committing — the trailing space is what makes `Mr.<space>Smith` not
// fire (the space hasn't streamed yet) and what makes `end.<space>`
// fire correctly.

import { useCallback, useEffect, useRef, useState } from "react";
import { decodeChatEvent } from "./chat";
import type { Lutin, TtsBackend, TtsStreamId } from "./lutin";

/// Index after the last terminator-followed-by-whitespace in `buf`,
/// or `-1` if none. Pulled out of `flush` so the boundary rule is
/// unit-testable without mocking the TTS surface; "ignore a trailing
/// terminator with no whitespace yet" is the bit that needs the most
/// scrutiny (it's what keeps `Mr.` and `e.g.` from splitting early).
export function lastSentenceEnd(buf: string): number {
  let lastEnd = -1;
  for (let i = 0; i < buf.length - 1; i++) {
    const c = buf[i];
    if (
      (c === "." || c === "!" || c === "?" || c === "\n") &&
      /\s/.test(buf[i + 1])
    ) {
      lastEnd = i + 1;
    }
  }
  return lastEnd;
}

const BACKEND: TtsBackend = {
  Orpheus: { model: "ThreeBQ4KM", voice: "Tara" },
};

/// Don't speak fragments shorter than this — keeps "Sure." or "OK."
/// from pre-empting the actual answer with a clipped chunk that has
/// no model warm-up window.
const MIN_FLUSH_LEN = 16;

export interface ChatTts {
  /// True while `ensureBackend` / `openStream` are in flight on the
  /// initial enable. UI shows a spinner; the toggle is non-functional
  /// until this clears.
  loading: boolean;
  /// Stop in-flight synthesis and discard the local sentence buffer.
  /// Hook this into the chat's own cancel button so a user-stop
  /// silences TTS too.
  cancel: () => void;
}

export function useChatTts(lutin: Lutin, enabled: boolean, speed: number = 1.0): ChatTts {
  const tts = lutin.tts;
  const streamRef = useRef<TtsStreamId | null>(null);
  const bufRef = useRef("");
  const [loading, setLoading] = useState(false);
  // Read latest speed inside the speak callback without retriggering
  // the broadcast subscription on every slider tick.
  const speedRef = useRef(speed);
  speedRef.current = speed;

  // Open the stream when enabled; close it on disable / unmount. We
  // don't try to keep the stream across an `enabled` toggle — the
  // close + reopen cost is negligible compared to a model load, and
  // it keeps the lifecycle obvious.
  useEffect(() => {
    if (!enabled || !tts) return;
    let cancelled = false;
    setLoading(true);
    (async () => {
      try {
        await tts.ensureBackend(BACKEND);
        if (cancelled) return;
        const id = await tts.openStream(BACKEND);
        if (cancelled) {
          tts.closeStream(id).catch((e) => console.warn("tts closeStream:", e));
          return;
        }
        streamRef.current = id;
      } catch (e) {
        // Surface to the console — the toggle just stays "loading"-
        // off; there's no UI affordance to recover yet, but at least
        // the failure isn't invisible.
        if (!cancelled) console.warn("tts enable failed:", e);
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
      const id = streamRef.current;
      streamRef.current = null;
      bufRef.current = "";
      if (id !== null) {
        tts.closeStream(id).catch((e) => console.warn("tts closeStream:", e));
      }
    };
  }, [enabled, tts]);

  // Two flush variants instead of `flush(force: boolean)` — the
  // branches diverge on both end-detection and the min-length rule,
  // so a boolean was just hiding two functions inside one body. Both
  // share `commit()` so the buf-update + speak path stays in one
  // place.
  const commit = useCallback(
    (chunk: string, rest: string) => {
      const id = streamRef.current;
      if (id === null || !tts) return;
      if (chunk.length === 0) return;
      bufRef.current = rest.replace(/^\s+/, "");
      tts.speak(id, chunk, { speed: speedRef.current }).catch((e) => console.warn("tts speak:", e));
    },
    [tts],
  );

  const flushSentences = useCallback(() => {
    const buf = bufRef.current;
    if (buf.length === 0) return;
    const lastEnd = lastSentenceEnd(buf);
    if (lastEnd <= 0) return;
    const chunk = buf.slice(0, lastEnd).trim();
    if (chunk.length < MIN_FLUSH_LEN) return;
    commit(chunk, buf.slice(lastEnd));
  }, [commit]);

  const flushAll = useCallback(() => {
    const buf = bufRef.current;
    const chunk = buf.trim();
    if (chunk.length === 0) return;
    commit(chunk, "");
  }, [commit]);

  // Parallel broadcast subscription. The App's main subscription
  // feeds the reducer; this one only listens for delta /
  // messageFinished and is unmounted when TTS toggles off.
  useEffect(() => {
    if (!enabled || !tts) return;
    const off = lutin.onBroadcast((body) => {
      let ev;
      try {
        ev = decodeChatEvent(body);
      } catch (e) {
        // A decode error here is a bug, not a recoverable condition —
        // log so it doesn't silently disable TTS for the rest of the
        // session.
        console.warn("tts: decodeChatEvent failed", e);
        return;
      }
      if (ev.kind === "delta") {
        bufRef.current += ev.text;
        flushSentences();
      } else if (ev.kind === "messageFinished") {
        flushAll();
      }
    });
    return off;
  }, [enabled, tts, lutin, flushSentences, flushAll]);

  const cancel = useCallback(() => {
    bufRef.current = "";
    const id = streamRef.current;
    // User-initiated stop: if the cancel call itself fails, the user
    // keeps hearing audio with no indication why — log loudly rather
    // than swallow.
    if (id !== null && tts) {
      tts.cancel(id).catch((e) => console.warn("tts cancel:", e));
    }
  }, [tts]);

  return { loading, cancel };
}
