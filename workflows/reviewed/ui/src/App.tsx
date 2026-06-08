// Chat for the reviewed workflow. Engine events are projected onto the
// shared chat-widgets model (see session.ts) and rendered through
// `<ChatView>`, so tool calls use the same rich widgets as the main chat
// (Edit diffs, file previews, Shell output). Principle verdicts are
// grafted onto each tool bubble by the ReviewedToolCall slot.

import { useCallback, useEffect, useMemo, useReducer, useState } from "react";
import { ChatView } from "@lutin/chat-widgets";
import {
  type ChatEvent,
  type PersonaInfo,
  decodeChatEvent,
  decodeChatResponse,
  encodeChatRequest,
} from "@lutin/reviewed-protocol";
import type { Lutin } from "./lutin";
import { initialSnapshot, reduce } from "./session";
import { makePersonaComposer } from "./PersonaComposer";
import { makeReviewedToolCall } from "./ReviewedToolCall";

interface Props {
  lutin: Lutin;
}

export function App({ lutin }: Props) {
  const [snap, dispatch] = useReducer(reduce, initialSnapshot);
  const [personas, setPersonas] = useState<PersonaInfo[] | null>(null);

  useEffect(() => {
    let cancelled = false;
    const off = lutin.onBroadcast((body) => {
      try {
        const event: ChatEvent = decodeChatEvent(body);
        dispatch({ type: "event", event });
      } catch (err) {
        console.warn("malformed ChatEvent broadcast", err);
      }
    });
    lutin
      .request(encodeChatRequest({ kind: "subscribe" }))
      .then((body) => {
        if (cancelled) return;
        const resp = decodeChatResponse(body);
        if (resp.ok && resp.value.kind === "subscribed") {
          dispatch({
            type: "loaded",
            persona: resp.value.state.persona,
            turns: resp.value.turns,
          });
        }
      })
      .catch((err) => console.warn("subscribe failed", err));
    lutin
      .request(encodeChatRequest({ kind: "listPersonas" }))
      .then((body) => {
        if (cancelled) return;
        const resp = decodeChatResponse(body);
        if (resp.ok && resp.value.kind === "personas") {
          setPersonas(resp.value.personas);
        }
      })
      .catch((err) => console.warn("listPersonas failed", err));
    return () => {
      cancelled = true;
      off();
    };
  }, [lutin]);

  const send = useCallback(
    (text: string) => {
      const t = text.trim();
      if (!t) return;
      dispatch({ type: "optimistic", text: t });
      lutin
        .request(encodeChatRequest({ kind: "sendMessage", text: t }))
        .catch((err) =>
          dispatch({ type: "submitFailed", message: `send: ${String(err)}` }),
        );
    },
    [lutin],
  );

  const cancel = useCallback(() => {
    lutin
      .request(encodeChatRequest({ kind: "cancel" }))
      .catch((err) => console.warn("cancel failed", err));
  }, [lutin]);

  const changePersona = useCallback(
    (name: string | null) => {
      lutin
        .request(encodeChatRequest({ kind: "setPersona", name }))
        .then((body) => {
          const resp = decodeChatResponse(body);
          if (resp.ok && resp.value.kind === "stateUpdated") {
            dispatch({
              type: "event",
              event: { kind: "stateChanged", state: resp.value.state },
            });
          }
        })
        .catch((err) => console.warn("setPersona failed", err));
    },
    [lutin],
  );

  const Composer = useMemo(
    () =>
      makePersonaComposer({
        personas,
        activePersona: snap.persona,
        onChangePersona: changePersona,
      }),
    [personas, snap.persona, changePersona],
  );

  const ToolCallSlot = useMemo(
    () =>
      makeReviewedToolCall({
        verdictsByCallId: snap.verdictsByCallId,
        stepIdByCallId: snap.stepIdByCallId,
        activeStepKeys: new Set(snap.activeStepKeys),
      }),
    [snap.verdictsByCallId, snap.stepIdByCallId, snap.activeStepKeys],
  );

  return (
    <ChatView
      messages={snap.messages}
      turn={snap.turn}
      onSend={send}
      onCancel={cancel}
      slots={{ Composer, ToolCall: ToolCallSlot }}
      className="lutin-reviewed"
    />
  );
}
