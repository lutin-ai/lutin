import { useState } from "react";
import {
  decodeImageResponse,
  encodeImageRequest,
  imageErrorMessage,
  type GeneratedImage,
} from "@lutin/image-protocol";
import type { Lutin } from "./lutin";

interface Turn {
  id: number;
  prompt: string;
  status: "pending" | "done" | "error";
  /// `null` while pending. Populated on success.
  image: GeneratedImage | null;
  /// `null` unless `status === "error"`.
  error: string | null;
}

const DEFAULT_SIZE = 1024;

export function App({ lutin }: { lutin: Lutin }) {
  const [prompt, setPrompt] = useState("");
  const [turns, setTurns] = useState<Turn[]>([]);
  const [busy, setBusy] = useState(false);
  let nextId = turns.length;

  const submit = async () => {
    const text = prompt.trim();
    if (!text || busy) return;
    const id = nextId++;
    const turn: Turn = {
      id,
      prompt: text,
      status: "pending",
      image: null,
      error: null,
    };
    setTurns((ts) => [...ts, turn]);
    setPrompt("");
    setBusy(true);
    try {
      const body = encodeImageRequest({
        kind: "generate",
        params: {
          prompt: text,
          seed: null,
          width: DEFAULT_SIZE,
          height: DEFAULT_SIZE,
        },
      });
      const respBytes = await lutin.request(body);
      const resp = decodeImageResponse(respBytes);
      setTurns((ts) =>
        ts.map((t) =>
          t.id !== id
            ? t
            : resp.ok
              ? { ...t, status: "done", image: resp.value.image }
              : { ...t, status: "error", error: imageErrorMessage(resp.error) },
        ),
      );
    } catch (e) {
      setTurns((ts) =>
        ts.map((t) =>
          t.id === id ? { ...t, status: "error", error: String(e) } : t,
        ),
      );
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        height: "100%",
        padding: "0.75rem",
        gap: "0.75rem",
      }}
    >
      <div
        style={{
          flex: 1,
          overflowY: "auto",
          display: "flex",
          flexDirection: "column",
          gap: "1rem",
        }}
      >
        {turns.length === 0 && (
          <div style={{ opacity: 0.5, alignSelf: "center", marginTop: "2rem" }}>
            Type a prompt below.
          </div>
        )}
        {turns.map((t) => (
          <TurnView key={t.id} turn={t} />
        ))}
      </div>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
        style={{ display: "flex", gap: "0.5rem" }}
      >
        <textarea
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              submit();
            }
          }}
          placeholder="Describe an image…"
          rows={2}
          disabled={busy}
          style={{
            flex: 1,
            background: "#1a1a1a",
            color: "#e8e8e8",
            border: "1px solid #333",
            borderRadius: 6,
            padding: "0.5rem",
            font: "inherit",
            resize: "none",
          }}
        />
        <button
          type="submit"
          disabled={busy || prompt.trim().length === 0}
          style={{
            background: busy ? "#333" : "#3b82f6",
            color: "white",
            border: "none",
            borderRadius: 6,
            padding: "0 1rem",
            cursor: busy ? "default" : "pointer",
            fontWeight: 600,
          }}
        >
          {busy ? "…" : "Generate"}
        </button>
      </form>
    </div>
  );
}

function TurnView({ turn }: { turn: Turn }) {
  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        gap: "0.4rem",
        background: "#181818",
        border: "1px solid #262626",
        borderRadius: 8,
        padding: "0.6rem 0.75rem",
      }}
    >
      <div style={{ fontSize: "0.85rem", opacity: 0.85 }}>{turn.prompt}</div>
      {turn.status === "pending" && (
        <div style={{ opacity: 0.6, fontSize: "0.85rem" }}>generating…</div>
      )}
      {turn.status === "error" && (
        <div style={{ color: "#f87171", fontSize: "0.85rem" }}>{turn.error}</div>
      )}
      {turn.status === "done" && turn.image && (
        <div>
          <img
            src={`data:${turn.image.mime};base64,${turn.image.bytesB64}`}
            alt={turn.prompt}
            style={{
              maxWidth: "100%",
              borderRadius: 6,
              display: "block",
            }}
          />
          <div
            style={{
              marginTop: "0.25rem",
              fontSize: "0.7rem",
              opacity: 0.5,
              fontVariantNumeric: "tabular-nums",
            }}
          >
            seed {String(turn.image.seed)} · {turn.image.ms} ms
          </div>
        </div>
      )}
    </div>
  );
}
