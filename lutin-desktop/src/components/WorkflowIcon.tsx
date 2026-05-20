import type { ReactElement } from "react";

// Hand-picked SVG icons per workflow id. We keep the set inline (vs.
// shipping an icon library) so each workflow gets a glyph tuned to
// the sidebar's 16px size — and so orphaned/unknown workflows can
// fall back to whatever emoji their manifest declared without a
// font-rendering mismatch.

interface Props {
  id: string;
  /** Emoji or short string from the workflow manifest; used when we
   *  don't have a bespoke SVG for this id. */
  fallback: string | null;
  size?: number;
}

export function WorkflowIcon({ id, fallback, size = 16 }: Props) {
  const Glyph = REGISTRY[id];
  if (Glyph) {
    return (
      <span className="lutin-wf-icon" aria-hidden style={{ width: size, height: size }}>
        <Glyph size={size} />
      </span>
    );
  }
  if (fallback) return <span aria-hidden>{fallback}</span>;
  return null;
}

type GlyphProps = { size: number };

const stroke = {
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.5,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
};

const REGISTRY: Record<string, (p: GlyphProps) => ReactElement> = {
  chat: ({ size }) => (
    <svg viewBox="0 0 16 16" width={size} height={size} {...stroke}>
      <path d="M2.5 4.5a1.5 1.5 0 0 1 1.5-1.5h8a1.5 1.5 0 0 1 1.5 1.5v5a1.5 1.5 0 0 1-1.5 1.5H6.5l-3 2.5v-2.5H4a1.5 1.5 0 0 1-1.5-1.5z" />
    </svg>
  ),
  principled: ({ size }) => (
    <svg viewBox="0 0 16 16" width={size} height={size} {...stroke}>
      <path d="M8 1.5 2.5 3.5v4.2c0 3 2.4 5.5 5.5 6.8 3.1-1.3 5.5-3.8 5.5-6.8V3.5z" />
      <path d="m5.5 8 2 2 3-4" />
    </svg>
  ),
  image: ({ size }) => (
    <svg viewBox="0 0 16 16" width={size} height={size} {...stroke}>
      <rect x="2" y="3" width="12" height="10" rx="1.5" />
      <circle cx="6" cy="7" r="1.2" />
      <path d="m2.5 12 3.5-3 3 3 2-2 2.5 2" />
    </svg>
  ),
  reviewed: ({ size }) => (
    <svg viewBox="0 0 16 16" width={size} height={size} {...stroke}>
      <path d="M1.5 8s2.3-4 6.5-4 6.5 4 6.5 4-2.3 4-6.5 4S1.5 8 1.5 8z" />
      <circle cx="8" cy="8" r="1.8" />
    </svg>
  ),
  scratchpad: ({ size }) => (
    <svg viewBox="0 0 16 16" width={size} height={size} {...stroke}>
      <path d="M3 2.5h7l3 3v8a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1v-10a1 1 0 0 1 1-1z" />
      <path d="M10 2.5v3h3" />
      <path d="M5 9h6M5 11.5h4" />
    </svg>
  ),
};
