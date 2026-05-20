import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import {
  decodeChatEvent,
  decodeChatResponse,
  encodeChatRequest,
} from "@lutin/scratchpad-protocol";
import { StepCard } from "./StepCard";
import type { Lutin } from "./lutin";
import { PersonaComposer } from "./PersonaComposer";
import { initialSnapshot, reduce } from "./session";

interface Props {
  lutin: Lutin;
}

export function App({ lutin }: Props) {
  const [snap, dispatch] = useReducer(reduce, initialSnapshot);
  const [draft, setDraft] = useState("");
  const transcriptRef = useRef<HTMLDivElement>(null);
  const pinnedToBottomRef = useRef(true);

  const onTranscriptScroll = useCallback(() => {
    const el = transcriptRef.current;
    if (!el) return;
    const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
    pinnedToBottomRef.current = distance < 32;
  }, []);

  useEffect(() => {
    if (!pinnedToBottomRef.current) return;
    transcriptRef.current?.scrollTo({ top: transcriptRef.current.scrollHeight });
  }, [snap.turns]);

  const refreshPersonas = useCallback(() => {
    lutin
      .request(encodeChatRequest({ kind: "listPersonas" }))
      .then((body) => {
        dispatch({ type: "response", response: decodeChatResponse(body) });
      })
      .catch(() => {});
  }, [lutin]);

  useEffect(() => {
    let cancelled = false;
    const off = lutin.onBroadcast((body) => {
      try {
        dispatch({ type: "event", event: decodeChatEvent(body) });
      } catch (err) {
        console.warn("malformed ChatEvent broadcast", err);
      }
    });

    lutin
      .request(encodeChatRequest({ kind: "subscribe" }))
      .then((body) => {
        if (cancelled) return;
        dispatch({ type: "response", response: decodeChatResponse(body) });
      })
      .catch((err) => {
        if (cancelled) return;
        dispatch({ type: "submitFailed", message: `subscribe: ${String(err)}` });
      });

    refreshPersonas();

    return () => {
      cancelled = true;
      off();
    };
  }, [lutin, refreshPersonas]);

  const send = useCallback(
    (text: string) => {
      const trimmed = text.trim();
      if (!trimmed) return;
      dispatch({ type: "submitOptimistic", text: trimmed });
      setDraft("");
      refreshPersonas();
      lutin
        .request(encodeChatRequest({ kind: "sendMessage", text: trimmed }))
        .then((body) => dispatch({ type: "response", response: decodeChatResponse(body) }))
        .catch((err) =>
          dispatch({ type: "submitFailed", message: `send: ${String(err)}` }),
        );
    },
    [lutin, refreshPersonas],
  );

  const cancel = useCallback(() => {
    lutin
      .request(encodeChatRequest({ kind: "cancel" }))
      .then((body) => dispatch({ type: "response", response: decodeChatResponse(body) }))
      .catch((err) =>
        dispatch({ type: "submitFailed", message: `cancel: ${String(err)}` }),
      );
  }, [lutin]);

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

  let stepIndex = -1;

  return (
    <div className="app">
      <div className="transcript" ref={transcriptRef} onScroll={onTranscriptScroll}>
        <div className="transcript-inner">
          {snap.turns.map((t) => {
            if (t.kind === "user") {
              return (
                <div key={t.id} className="row row--user">
                  <div className="user-bubble">{t.text}</div>
                </div>
              );
            }
            if (t.kind === "assistant") {
              return (
                <div key={t.id} className="row row--assistant">
                  <div className="assistant-bubble">{t.text}</div>
                </div>
              );
            }
            stepIndex++;
            return (
              <div key={t.id} className="row row--step">
                <StepCard index={stepIndex} step={t.step} />
              </div>
            );
          })}
          {snap.error && (
            <div className="row row--error">
              <div className="error-banner">{snap.error}</div>
            </div>
          )}
        </div>
      </div>
      <PersonaComposer
        value={draft}
        onChange={setDraft}
        onSubmit={() => send(draft)}
        onCancel={cancel}
        busy={snap.inFlight}
        disabled={snap.inFlight}
        personas={snap.personas}
        activePersona={snap.state.persona}
        onChangePersona={changePersona}
      />
    </div>
  );
}
