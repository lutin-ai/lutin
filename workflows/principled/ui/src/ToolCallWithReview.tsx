// Principled-specific ToolCall slot. Wraps the chat-widgets default
// renderer with two pieces of principled-only chrome:
//
//   1. An inline reviewer panel under the bubble showing every verdict
//      that scored this attempt (matched by `callId`). Default filter
//      hides clean `pass` rows so the panel is empty for boring-good
//      attempts; toggles let the user reveal them.
//
//   2. An iteration-box outline when the bubble's `callId` belongs to
//      a step that's still `Active` in the engine. The outline groups
//      every attempt for that step (multiple Fix-retries share a step
//      but get distinct callIds) so the user can see what's being
//      iterated on at a glance. Drops back to a plain bubble when the
//      step resolves.

import { useMemo, useState } from "react";
import { ToolCall as DefaultToolCall } from "@lutin/chat-widgets";
import type { ToolCallProps } from "@lutin/chat-widgets";
import type { ReviewLogEntry, ReviewVerdict } from "@lutin/principled-protocol";

export interface ReviewContext {
  /** `callId` → verdict rows scored against that attempt. */
  verdictsByCallId: Record<string, ReviewLogEntry[]>;
  /** `callId` → `stepId` for the step the attempt belongs to. */
  stepIdByCallId: Record<string, string>;
  /** `stepId`s of currently-Active frames. Used to draw the outline. */
  activeStepIds: ReadonlySet<string>;
}

type FilterMode = "default" | "all";

/// Build a ToolCall slot bound to the current review context. Returned
/// component goes straight into `<ChatView slots={{ ToolCall: ... }} />`.
/// The closure means the slot re-renders whenever the wrapping component
/// passes a new context — the caller is expected to memoize the context
/// alongside its snapshot so identity stays stable across unrelated
/// renders.
export function makeToolCallWithReview(
  ctx: ReviewContext,
): React.ComponentType<ToolCallProps> {
  return function ToolCallWithReview(props: ToolCallProps) {
    const callId = props.message.id;
    const verdicts = ctx.verdictsByCallId[callId] ?? [];
    const stepId = ctx.stepIdByCallId[callId];
    const isActive = stepId != null && ctx.activeStepIds.has(stepId);

    const wrapClass = isActive
      ? "lutin-principled__iteration lutin-principled__iteration--active"
      : "lutin-principled__iteration";

    return (
      <div className={wrapClass}>
        <DefaultToolCall {...props} />
        {verdicts.length > 0 && <InlineReviewPanel verdicts={verdicts} />}
      </div>
    );
  };
}

function InlineReviewPanel({ verdicts }: { verdicts: ReviewLogEntry[] }) {
  const [mode, setMode] = useState<FilterMode>("default");
  const visible = useMemo(() => {
    if (mode === "all") return verdicts;
    // Default: show fail / pass-with-nit (anything with reasoning to
    // surface). Plain `pass` rows clutter without informing.
    return verdicts.filter((v) => v.verdict.kind !== "pass");
  }, [verdicts, mode]);

  const counts = useMemo(() => countByKind(verdicts), [verdicts]);
  const hasHidden = mode === "default" && counts.pass > 0;

  // Headline picks whichever count is most informative — failures
  // dominate, nits matter when there are no failures, otherwise we
  // just say "all pass".
  const total = counts.pass + counts.passWithNit + counts.fail;
  const headline =
    counts.fail > 0
      ? `${counts.fail} failing · ${total - counts.fail} other`
      : counts.passWithNit > 0
        ? `${counts.passWithNit} note${counts.passWithNit === 1 ? "" : "s"} · ${counts.pass} clean`
        : `${total} reviewer${total === 1 ? "" : "s"} pass`;

  return (
    <div className="lutin-principled__review">
      <div className="lutin-principled__review-head">
        <span className="lutin-principled__review-summary">{headline}</span>
        {hasHidden && (
          <button
            type="button"
            className="lutin-principled__review-toggle"
            onClick={() => setMode("all")}
          >
            show {counts.pass} pass{counts.pass === 1 ? "" : "es"}
          </button>
        )}
        {mode === "all" && counts.pass > 0 && (
          <button
            type="button"
            className="lutin-principled__review-toggle"
            onClick={() => setMode("default")}
          >
            hide passes
          </button>
        )}
      </div>
      {visible.length > 0 && (
        <ul className="lutin-principled__review-rows">
          {visible.map((row) => {
            // Reasoning lives only on `passWithNit` and `fail`; plain
            // `pass` has nothing to surface (and is filtered out of
            // `visible` anyway when in default mode).
            const reasoning =
              row.verdict.kind === "pass" ? null : row.verdict.reasoning;
            return (
              <li
                key={`${row.reviewerCallId.toString()}-${row.principle}`}
                className="lutin-principled__review-row"
                data-kind={row.verdict.kind}
              >
                <span className="lutin-principled__review-principle">
                  {row.principle}
                </span>
                <VerdictBadge verdict={row.verdict} />
                {reasoning && (
                  <span className="lutin-principled__review-reason">
                    {reasoning}
                  </span>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

interface VerdictCounts {
  pass: number;
  passWithNit: number;
  fail: number;
}

function countByKind(rows: ReviewLogEntry[]): VerdictCounts {
  const out: VerdictCounts = { pass: 0, passWithNit: 0, fail: 0 };
  for (const r of rows) {
    if (r.verdict.kind === "pass") out.pass += 1;
    else if (r.verdict.kind === "passWithNit") out.passWithNit += 1;
    else out.fail += 1;
  }
  return out;
}

function VerdictBadge({ verdict }: { verdict: ReviewVerdict }) {
  switch (verdict.kind) {
    case "pass":
      return <span className="lutin-principled__verdict lutin-principled__verdict--pass">pass</span>;
    case "passWithNit":
      return (
        <span className="lutin-principled__verdict lutin-principled__verdict--nit">
          nit
        </span>
      );
    case "fail":
      return (
        <span
          className="lutin-principled__verdict lutin-principled__verdict--fail"
          data-severity={verdict.severity.kind}
        >
          {verdict.severity.kind === "rethink" ? "rethink" : "fix"}
        </span>
      );
  }
}
