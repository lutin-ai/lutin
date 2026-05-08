import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { ChatView } from "@lutin/chat-widgets";
import "@lutin/chat-widgets/theme.css";
import type { Lutin } from "./lutin";
import {
  type ChatEvent,
  type ChatResponse,
  type PersonaInfo,
  decodeChatEvent,
  decodeChatResponse,
  encodeChatRequest,
} from "./chat";
import { initialSnapshot, reduce } from "./session";
import { toViewModel } from "./adapter";
import { makePersonaComposer } from "./PersonaComposer";
import { useChatTts } from "./tts";

interface Props {
  lutin: Lutin;
}

export function App({ lutin }: Props) {
  const [snap, dispatch] = useReducer(reduce, initialSnapshot);
  const [personas, setPersonas] = useState<PersonaInfo[] | null>(null);
  const [draft, setDraft] = useState("");
  const [ttsOn, setTtsOn] = useState(false);
  const tts = useChatTts(lutin, ttsOn);

  // Wire PTT / open-mic transcription deliveries into the composer.
  // We append rather than replace so the user can stack voice input
  // on top of already-typed text. The plan calls for "don't auto-send"
  // — the user reviews the result before hitting send.
  useEffect(() => {
    if (!lutin.onTranscription) return;
    const off = lutin.onTranscription(({ text }) => {
      if (!text) return;
      setDraft((prev) => (prev.length === 0 ? text : `${prev} ${text}`));
    });
    return off;
  }, [lutin]);

  // Subscribe to engine broadcasts and fetch personas. Personas are
  // engine-side metadata (file enumeration in `personas/`) so we
  // request once on mount; if the user adds a new persona file the
  // page is reloaded today. Hot-reload would need a CP broadcast.
  useEffect(() => {
    let cancelled = false;
    const off = lutin.onBroadcast((body) => {
      let ev: ChatEvent;
      try {
        ev = decodeChatEvent(body);
      } catch (err) {
        console.warn("malformed ChatEvent broadcast", err);
        return;
      }
      dispatch({ type: "event", event: ev });
    });

    lutin
      .request(encodeChatRequest({ kind: "subscribe" }))
      .then((body) => {
        if (cancelled) return;
        let resp: ChatResponse;
        try {
          resp = decodeChatResponse(body);
        } catch (err) {
          console.warn("malformed Subscribe response", err);
          return;
        }
        dispatch({ type: "response", response: resp });
      })
      .catch((err) => {
        if (cancelled) return;
        dispatch({ type: "submitFailed", message: `subscribe: ${String(err)}` });
      });

    lutin
      .request(encodeChatRequest({ kind: "listPersonas" }))
      .then((body) => {
        if (cancelled) return;
        const resp = decodeChatResponse(body);
        if (resp.ok && resp.value.kind === "personas") {
          setPersonas(resp.value.personas);
        }
      })
      .catch(() => {
        // Persona picker degrades gracefully; failure here just leaves
        // the dropdown showing "no personas configured".
        if (!cancelled) setPersonas([]);
      });

    return () => {
      cancelled = true;
      off();
    };
  }, [lutin]);

  // Pending sends/reruns that arrived while another turn was in flight.
  // Drained on the next idle tick so the user can stack up follow-ups
  // without waiting for each turn to complete.
  type PendingItem = { kind: "send"; text: string } | { kind: "rerun" };
  const queueRef = useRef<PendingItem[]>([]);

  const dispatchSend = useCallback(
    (text: string) => {
      dispatch({ type: "submitOptimistic", text });
      lutin
        .request(encodeChatRequest({ kind: "sendMessage", text }))
        .then((body) => {
          let resp: ChatResponse;
          try {
            resp = decodeChatResponse(body);
          } catch (err) {
            dispatch({
              type: "submitFailed",
              message: `decode SendMessage response: ${String(err)}`,
            });
            return;
          }
          dispatch({ type: "response", response: resp });
        })
        .catch((err) => {
          dispatch({ type: "submitFailed", message: String(err) });
        });
    },
    [lutin],
  );

  const dispatchRerun = useCallback(() => {
    dispatch({ type: "rerunOptimistic" });
    lutin
      .request(encodeChatRequest({ kind: "rerun" }))
      .then((body) => {
        let resp: ChatResponse;
        try {
          resp = decodeChatResponse(body);
        } catch (err) {
          dispatch({
            type: "submitFailed",
            message: `decode Rerun response: ${String(err)}`,
          });
          return;
        }
        dispatch({ type: "response", response: resp });
      })
      .catch((err) => {
        dispatch({ type: "submitFailed", message: String(err) });
      });
  }, [lutin]);

  const send = useCallback(
    (text: string) => {
      if (snap.turn.kind === "streaming") {
        queueRef.current.push({ kind: "send", text });
        return;
      }
      dispatchSend(text);
    },
    [snap.turn.kind, dispatchSend],
  );

  const rerun = useCallback(() => {
    if (snap.turn.kind === "streaming") {
      queueRef.current.push({ kind: "rerun" });
      return;
    }
    dispatchRerun();
  }, [snap.turn.kind, dispatchRerun]);

  // Drain one queued item per idle tick. Process FIFO; the dispatched
  // call flips the turn back to streaming so we re-enter this effect
  // when it lands again.
  useEffect(() => {
    if (snap.turn.kind !== "idle") return;
    const next = queueRef.current.shift();
    if (!next) return;
    if (next.kind === "send") dispatchSend(next.text);
    else dispatchRerun();
  }, [snap.turn.kind, dispatchSend, dispatchRerun]);

  const cancel = useCallback(() => {
    // Drop any queued follow-ups too — Cancel means "stop, don't roll
    // straight into the next thing I queued."
    queueRef.current.length = 0;
    lutin.request(encodeChatRequest({ kind: "cancel" })).catch(() => {});
    // Silence in-flight TTS at the same time. Without this the user
    // hears the rest of the last queued sentence after pressing stop.
    tts.cancel();
  }, [lutin, tts]);

  const changePersona = useCallback(
    (name: string | null) => {
      lutin
        .request(encodeChatRequest({ kind: "setPersona", name }))
        .then((body) => dispatch({ type: "response", response: decodeChatResponse(body) }))
        .catch((err) =>
          dispatch({ type: "submitFailed", message: `setPersona: ${String(err)}` }),
        );
    },
    [lutin],
  );

  const ttsAvailable = lutin.tts !== undefined;
  const onToggleTts = useCallback(() => setTtsOn((v) => !v), []);

  const Composer = useMemo(
    () =>
      makePersonaComposer({
        personas,
        activePersona: snap.persona,
        onChangePersona: changePersona,
        onRerun: rerun,
        ttsAvailable,
        ttsOn,
        ttsLoading: tts.loading,
        onToggleTts,
      }),
    [personas, snap.persona, changePersona, rerun, ttsAvailable, ttsOn, tts.loading, onToggleTts],
  );

  const vm = toViewModel(snap);

  return (
    <ChatView
      messages={vm.messages}
      turn={vm.turn}
      onSend={send}
      onCancel={cancel}
      draft={draft}
      onDraftChange={setDraft}
      slots={{ Composer }}
    />
  );
}
