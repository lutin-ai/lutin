import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { ChatView, appendComposerText, type MessageActions } from "@lutin/chat-widgets";
import "@lutin/chat-widgets/theme.css";
import type { Lutin } from "./lutin";
import {
  type ChatEvent,
  type ChatRequest,
  type ChatResponse,
  type PersonaInfo,
  decodeChatEvent,
  decodeChatResponse,
  encodeChatRequest,
} from "@lutin/chat-protocol";
import { type Message, initialSnapshot, reduce } from "./session";

// Mirror of the engine's `summary.json::title` derivation. The
// truncation cap matches Rust `SUMMARY_TITLE_CHARS` so the live
// sidebar label and the on-disk fallback agree on what they show.
const SUMMARY_TITLE_CHARS = 80;
function deriveTitle(completed: Message[]): string | null {
  for (const m of completed) {
    if (m.role !== "user") continue;
    const t = m.text.trim();
    if (!t) continue;
    return t.length > SUMMARY_TITLE_CHARS ? `${t.slice(0, SUMMARY_TITLE_CHARS - 1)}…` : t;
  }
  return null;
}
import { subAgentViewModel, toViewModel } from "./adapter";
import { makePersonaComposer } from "./PersonaComposer";
import { useChatTts } from "./tts";

interface Props {
  lutin: Lutin;
}

export function App({ lutin }: Props) {
  const [snap, dispatch] = useReducer(reduce, initialSnapshot);
  const [personas, setPersonas] = useState<PersonaInfo[] | null>(null);
  // `null` = parent session view; `agent#N` = read-only child transcript.
  // The desktop sidebar drives this — chat doesn't render its own
  // sub-agent picker anymore. We mirror the chrome's selection in
  // local state so the transcript swap stays a React render.
  const [selectedAgent, setSelectedAgent] = useState<string | null>(null);
  const [ttsOn, setTtsOn] = useState(false);
  const [ttsSpeed, setTtsSpeed] = useState(1.0);
  const tts = useChatTts(lutin, ttsOn, ttsSpeed);

  // Wire PTT / open-mic transcription deliveries into the composer.
  // The composer owns its own draft state — going through a window
  // CustomEvent keeps re-renders local to the composer subtree instead
  // of re-rendering the entire transcript on every voice-input append
  // (which is the same reason the composer doesn't lift `draft` up).
  useEffect(() => {
    if (!lutin.onTranscription) return;
    const off = lutin.onTranscription(({ text }) => {
      appendComposerText(text);
    });
    return off;
  }, [lutin]);

  // Refetch the persona list. Engine reads TOML fresh from disk on
  // each `ListPersonas`, so this picks up out-of-band edits to
  // display_name / model and any added/removed persona files. Called
  // on mount and again on each send so the dropdown chip stays
  // aligned with what the engine will actually use on that turn
  // (`engine.rs` already reloads the persona TOML in `run_turn`).
  const refreshPersonas = useCallback(() => {
    lutin
      .request(encodeChatRequest({ kind: "listPersonas" }))
      .then((body) => {
        const resp = decodeChatResponse(body);
        if (resp.ok && resp.value.kind === "personas") {
          setPersonas(resp.value.personas);
        }
      })
      .catch(() => {
        // Persona picker degrades gracefully; keep the previous list
        // rather than blanking it on a transient error.
      });
  }, [lutin]);

  // Subscribe to engine broadcasts and fetch personas.
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

    // Metrics ride a separate request because Subscribed predates the
    // sidecar; sending it after Subscribe keeps wire shapes additive.
    lutin
      .request(encodeChatRequest({ kind: "getMetrics" }))
      .then((body) => {
        if (cancelled) return;
        try {
          dispatch({ type: "response", response: decodeChatResponse(body) });
        } catch (err) {
          console.warn("malformed GetMetrics response", err);
        }
      })
      .catch(() => {
        // Metrics are decorative — failure leaves footers blank.
      });

    lutin
      .request(encodeChatRequest({ kind: "listSubAgents" }))
      .then((body) => {
        if (cancelled) return;
        try {
          dispatch({ type: "response", response: decodeChatResponse(body) });
        } catch (err) {
          console.warn("malformed ListSubAgents response", err);
        }
      })
      .catch(() => {
        // Panel is decorative — failure leaves it empty.
      });

    refreshPersonas();

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
      // Refresh persona metadata in parallel with the send. The engine
      // reloads the persona TOML on every turn, so the chip/model label
      // would otherwise drift from what the LLM is actually using.
      refreshPersonas();
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

  // Chrome (desktop sidebar) drives sub-agent selection. We mirror it
  // into local state and pull a fresh transcript for non-null ids —
  // the live broadcast keeps an open child warm, but one that finished
  // while it wasn't selected needs a pull to surface its terminal turn.
  // The capability gate makes these optional at type-level; chat
  // declares `"sub_agents"` so they're always present at runtime.
  useEffect(() => {
    if (!lutin.onSelectSubAgent) return;
    const off = lutin.onSelectSubAgent((id) => {
      setSelectedAgent(id);
      if (id === null) return;
      lutin
        .request(encodeChatRequest({ kind: "getSubAgentTranscript", id }))
        .then((body) => {
          try {
            dispatch({ type: "response", response: decodeChatResponse(body) });
          } catch (err) {
            console.warn("malformed GetSubAgentTranscript response", err);
          }
        })
        .catch(() => {
          // Read-only side panel — a missing transcript renders empty.
        });
    });
    return off;
  }, [lutin]);

  // Mirror the live sub-agent registry up to the chrome so its
  // sidebar can render the tree. The chat workflow is the
  // authoritative source; the desktop just displays. Sending the
  // full list each time keeps the wire shape boringly idempotent.
  // `SubAgentInfo` and `SubAgentRow` are structurally identical
  // (chat's postcard schema vs the cross-boundary shim type), so
  // structural typing covers the assignment.
  useEffect(() => {
    lutin.publishSubAgents?.(snap.subAgents);
  }, [lutin, snap.subAgents]);

  // Mirror live session metadata up to the chrome on every change so
  // the sidebar's `title`, `persona`, and `ctx` columns update as the
  // agent loop runs — not only at end-of-turn. Token values are 0 /
  // null until the first provider Usage report lands, which matches
  // the on-disk `summary.json` invariant (no usage = no totals yet).
  // Persona + title piggyback on the same channel so the sidebar
  // reflects persona switches and the first user message immediately.
  useEffect(() => {
    const title = deriveTitle(snap.completed);
    const tokens = snap.summary ?? {
      contextTokens: null,
      totalPromptTokens: 0,
      totalCompletionTokens: 0,
    };
    // Omit `title` when we can't derive one (transcript not yet
    // loaded after a remount). `null` would tell the chrome to clear
    // the persisted title; `undefined` leaves it intact, so the
    // sidebar keeps showing the on-disk title until we have something
    // authoritative to publish.
    lutin.publishSummary({
      ...tokens,
      persona: snap.persona,
      ...(title !== null ? { title } : {}),
    });
  }, [lutin, snap.summary, snap.persona, snap.completed]);

  const ttsAvailable = lutin.tts !== undefined;
  const onToggleTts = useCallback(() => setTtsOn((v) => !v), []);

  // Right-click context-menu actions on completed messages. The bubble
  // ids are stringified projected indices (see adapter.ts); ignore
  // non-numeric ids (live/flushed streaming buffers).
  const messageActions = useMemo<MessageActions>(() => {
    const send = (req: ChatRequest, label: string) => {
      lutin
        .request(encodeChatRequest(req))
        .then((body) => dispatch({ type: "response", response: decodeChatResponse(body) }))
        .catch((err) =>
          dispatch({ type: "submitFailed", message: `${label}: ${String(err)}` }),
        );
    };
    const parseIndex = (id: string): number | null => {
      const n = Number(id);
      return Number.isInteger(n) && n >= 0 ? n : null;
    };
    return {
      onEdit: (id, text) => {
        const index = parseIndex(id);
        if (index !== null) send({ kind: "editMessage", index, text }, "editMessage");
      },
      onDelete: (id) => {
        const index = parseIndex(id);
        if (index !== null) send({ kind: "deleteMessage", index }, "deleteMessage");
      },
      onDeleteFromHere: (id) => {
        const index = parseIndex(id);
        if (index !== null) send({ kind: "deleteFromHere", index }, "deleteFromHere");
      },
    };
  }, [lutin]);

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
        ttsSpeed,
        onChangeTtsSpeed: setTtsSpeed,
        summary: snap.summary,
      }),
    [personas, snap.persona, snap.summary, changePersona, rerun, ttsAvailable, ttsOn, tts.loading, onToggleTts, ttsSpeed],
  );

  // Hide the composer entirely when viewing a child — sub-agents are
  // read-only here. We swap to a no-op slot rather than disabling the
  // composer so the chat-widget's spacing collapses cleanly.
  const HiddenComposer = useMemo(() => () => null, []);

  const viewing = selectedAgent;
  // Memoize the projected view model so re-renders that don't touch
  // the snapshot don't allocate a fresh `messages` array and `turn`
  // object. Without this, ChatView's `useScrollStick` deps trip on
  // every render and force a `scrollTop = scrollHeight` reflow.
  const vm = useMemo(
    () =>
      viewing === null
        ? toViewModel(snap)
        : subAgentViewModel(snap.subAgentTranscripts[viewing] ?? []),
    [viewing, snap],
  );
  const composerSlot = viewing === null ? Composer : HiddenComposer;

  return (
    <ChatView
      messages={vm.messages}
      turn={vm.turn}
      onSend={send}
      onCancel={cancel}
      messageActions={messageActions}
      slots={{ Composer: composerSlot }}
    />
  );
}

