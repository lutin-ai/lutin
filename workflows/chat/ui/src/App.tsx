import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { ChatView, type MessageActions } from "@lutin/chat-widgets";
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
} from "./chat";
import { initialSnapshot, reduce } from "./session";
import { subAgentViewModel, toViewModel } from "./adapter";
import type { SubAgentInfo } from "./chat";
import { makePersonaComposer } from "./PersonaComposer";
import { useChatTts } from "./tts";

interface Props {
  lutin: Lutin;
}

export function App({ lutin }: Props) {
  const [snap, dispatch] = useReducer(reduce, initialSnapshot);
  const [personas, setPersonas] = useState<PersonaInfo[] | null>(null);
  const [draft, setDraft] = useState("");
  // `null` = parent session view; `agent#N` = read-only child transcript.
  // Drives both the rendered transcript and the composer's visibility.
  const [selectedAgent, setSelectedAgent] = useState<string | null>(null);
  const [ttsOn, setTtsOn] = useState(false);
  const [ttsSpeed, setTtsSpeed] = useState(1.0);
  const tts = useChatTts(lutin, ttsOn, ttsSpeed);

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

  const selectAgent = useCallback(
    (id: string | null) => {
      setSelectedAgent(id);
      if (id === null) return;
      // Fetch a fresh snapshot every time the user opens (or re-opens)
      // a child — the live broadcast keeps it warm while open, but a
      // child that finished while the panel was closed needs a pull
      // to land its terminal turn.
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
    },
    [lutin],
  );

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
      }),
    [personas, snap.persona, changePersona, rerun, ttsAvailable, ttsOn, tts.loading, onToggleTts, ttsSpeed],
  );

  // Hide the composer entirely when viewing a child — sub-agents are
  // read-only here. We swap to a no-op slot rather than disabling the
  // composer so the chat-widget's spacing collapses cleanly.
  const HiddenComposer = useMemo(() => () => null, []);

  const viewing = selectedAgent;
  const vm =
    viewing === null
      ? toViewModel(snap)
      : subAgentViewModel(snap.subAgentTranscripts[viewing] ?? []);
  const composerSlot = viewing === null ? Composer : HiddenComposer;

  return (
    <div style={{ display: "flex", height: "100%", minHeight: 0 }}>
      <SubAgentsTree
        agents={snap.subAgents}
        selected={selectedAgent}
        onSelect={selectAgent}
      />
      <div style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column" }}>
        {viewing !== null && (
          <ChildHeader
            agent={snap.subAgents.find((a) => a.id === viewing) ?? null}
            id={viewing}
            onBack={() => selectAgent(null)}
          />
        )}
        <div style={{ flex: 1, minHeight: 0, display: "flex" }}>
          <ChatView
            messages={vm.messages}
            turn={vm.turn}
            onSend={send}
            onCancel={cancel}
            draft={draft}
            onDraftChange={setDraft}
            messageActions={messageActions}
            slots={{ Composer: composerSlot }}
          />
        </div>
      </div>
    </div>
  );
}

interface TreeNode {
  agent: SubAgentInfo;
  children: TreeNode[];
}

/// Build a forest of sub-agent rows out of the flat `parentId` list.
/// Top-level entries are children of the parent session (parentId
/// null); deeper levels nest under their parent's id. Orphaned rows
/// (parent gone before snapshot landed) get hoisted to top-level so
/// they don't disappear from the tree.
function buildTree(agents: SubAgentInfo[]): TreeNode[] {
  const byId = new Map<string, TreeNode>(
    agents.map((a) => [a.id, { agent: a, children: [] }]),
  );
  const roots: TreeNode[] = [];
  for (const a of agents) {
    const node = byId.get(a.id)!;
    if (a.parentId !== null && byId.has(a.parentId)) {
      byId.get(a.parentId)!.children.push(node);
    } else {
      roots.push(node);
    }
  }
  return roots;
}

interface FlatRow {
  agent: SubAgentInfo;
  /// Stable prefix string of the parent column ("│ ", "  ") chars in
  /// order from root → this row's parent. The row's own connector
  /// (├─ / └─) is appended at render.
  prefix: string;
  isLast: boolean;
}

function flatten(tree: TreeNode[], prefix = "", out: FlatRow[] = []): FlatRow[] {
  tree.forEach((node, i) => {
    const isLast = i === tree.length - 1;
    out.push({ agent: node.agent, prefix, isLast });
    flatten(node.children, prefix + (isLast ? "  " : "│ "), out);
  });
  return out;
}

interface TreeProps {
  agents: SubAgentInfo[];
  selected: string | null;
  onSelect: (id: string | null) => void;
}

function SubAgentsTree({ agents, selected, onSelect }: TreeProps) {
  const rows = flatten(buildTree(agents));
  return (
    <aside
      style={{
        width: 260,
        flexShrink: 0,
        borderRight: "1px solid rgba(255,255,255,0.08)",
        background: "rgba(255,255,255,0.02)",
        color: "#ddd",
        font: "12px/1.4 -apple-system, system-ui, sans-serif",
        padding: "12px 0",
        overflowY: "auto",
      }}
    >
      <div
        style={{
          textTransform: "uppercase",
          letterSpacing: "0.06em",
          fontSize: 10,
          color: "#888",
          padding: "0 14px 8px",
        }}
      >
        Agents
      </div>
      <RootRow selected={selected === null} onSelect={() => onSelect(null)} />
      {rows.length === 0 ? (
        <div style={{ color: "#555", padding: "8px 14px", fontSize: 11 }}>
          (no sub-agents yet)
        </div>
      ) : (
        rows.map((row) => (
          <TreeRow
            key={row.agent.id}
            row={row}
            selected={selected === row.agent.id}
            onSelect={() => onSelect(row.agent.id)}
          />
        ))
      )}
    </aside>
  );
}

function RootRow({ selected, onSelect }: { selected: boolean; onSelect: () => void }) {
  return (
    <button
      onClick={onSelect}
      style={{
        display: "flex",
        alignItems: "center",
        gap: 8,
        width: "100%",
        textAlign: "left",
        background: selected ? "rgba(255,255,255,0.06)" : "transparent",
        color: "#ddd",
        border: "none",
        padding: "5px 14px",
        cursor: "pointer",
        fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
        fontSize: 12,
      }}
    >
      <FolderIcon />
      <span>chat</span>
    </button>
  );
}

function TreeRow({
  row,
  selected,
  onSelect,
}: {
  row: FlatRow;
  selected: boolean;
  onSelect: () => void;
}) {
  const connector = row.isLast ? "└─" : "├─";
  const { agent } = row;
  return (
    <button
      onClick={onSelect}
      title={
        agent.status.kind === "failed"
          ? agent.status.reason
          : (agent.lastProgress ?? "")
      }
      style={{
        display: "flex",
        alignItems: "center",
        gap: 6,
        width: "100%",
        textAlign: "left",
        background: selected ? "rgba(255,255,255,0.06)" : "transparent",
        color: "#ddd",
        border: "none",
        padding: "4px 14px",
        cursor: "pointer",
        fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
        fontSize: 12,
        whiteSpace: "pre",
      }}
    >
      <span style={{ color: "#555" }}>
        {row.prefix}
        {connector}
      </span>
      <StatusDot status={agent.status} />
      <span style={{ color: "#aaa" }}>{agent.id}</span>
      <span
        style={{
          color: "#777",
          fontFamily: "-apple-system, system-ui, sans-serif",
          fontSize: 11,
          marginLeft: 4,
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
          flex: 1,
        }}
      >
        {agent.persona}
      </span>
    </button>
  );
}

function FolderIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 16 16" aria-hidden style={{ flexShrink: 0 }}>
      <path
        d="M1.5 4.5a1 1 0 0 1 1-1h3.2a1 1 0 0 1 .7.3l1 1h6.1a1 1 0 0 1 1 1v6.7a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1z"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.4"
      />
    </svg>
  );
}

function StatusDot({ status }: { status: SubAgentInfo["status"] }) {
  const color = (() => {
    switch (status.kind) {
      case "running":
        return "#5cf";
      case "completed":
        return "#7d6";
      case "failed":
        return "#f77";
      case "stopped":
        return "#999";
    }
  })();
  return (
    <span
      style={{
        width: 7,
        height: 7,
        borderRadius: "50%",
        background: color,
        flexShrink: 0,
      }}
    />
  );
}

function ChildHeader({
  agent,
  id,
  onBack,
}: {
  agent: SubAgentInfo | null;
  id: string;
  onBack: () => void;
}) {
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: 10,
        padding: "8px 16px",
        borderBottom: "1px solid rgba(255,255,255,0.06)",
        font: "12px/1.4 -apple-system, system-ui, sans-serif",
        color: "#bbb",
        background: "rgba(255,255,255,0.02)",
      }}
    >
      <button
        onClick={onBack}
        style={{
          background: "rgba(255,255,255,0.05)",
          border: "1px solid rgba(255,255,255,0.08)",
          color: "#ddd",
          borderRadius: 4,
          padding: "3px 8px",
          cursor: "pointer",
          fontSize: 11,
        }}
      >
        ← chat
      </button>
      <span style={{ fontFamily: "ui-monospace, monospace" }}>{id}</span>
      {agent && <span style={{ color: "#888" }}>· {agent.persona}</span>}
      {agent && <StatusDot status={agent.status} />}
      {agent && agent.status.kind === "failed" && (
        <span style={{ color: "#e88" }} title={agent.status.reason}>
          failed
        </span>
      )}
    </div>
  );
}
