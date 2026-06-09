// ToolCall slot for the reviewed workflow. Wraps the chat-widgets
// default renderer with two pieces of reviewed-only chrome:
//
//   1. A principle panel under the bubble: a chip per verdict scored
//      against this attempt (matched by `callId`). Passes show name-only;
//      failures expand to the blocking feedback; skips note the streak.
//
//   2. An active-step outline while the bubble's step is still under
//      review (drafted, not yet executed). Groups every attempt bubble
//      for that step; drops to a plain bubble once the step resolves.

import { ToolCall as DefaultToolCall } from "@lutin/chat-widgets";
import type { ToolCallProps } from "@lutin/chat-widgets";
import type { Check } from "./session";

export interface ReviewContext {
  verdictsByCallId: Record<string, Check[]>;
  stepIdByCallId: Record<string, string>;
  activeStepKeys: ReadonlySet<string>;
}

export function makeReviewedToolCall(
  ctx: ReviewContext,
): React.ComponentType<ToolCallProps> {
  return function ReviewedToolCall(props: ToolCallProps) {
    const callId = props.message.id;
    const checks = ctx.verdictsByCallId[callId] ?? [];
    const stepKey = ctx.stepIdByCallId[callId];
    const active = stepKey != null && ctx.activeStepKeys.has(stepKey);

    return (
      <div
        className={
          active
            ? "lutin-reviewed__step lutin-reviewed__step--active"
            : "lutin-reviewed__step"
        }
      >
        {props.message.name === "create_task" ? (
          <CreateTaskCall message={props.message} />
        ) : (
          <DefaultToolCall {...props} />
        )}
        {checks.length > 0 && <PrinciplePanel checks={checks} />}
      </div>
    );
  };
}

// create_task is pure plumbing — a full args/output widget is noise.
// Render the standard tool header (dot + name) with the task title as the
// summary, so it matches every other tool bubble; no expandable body.
function CreateTaskCall({ message }: { message: ToolCallProps["message"] }) {
  let title: string | null = null;
  if (
    message.args.kind === "parsed" &&
    message.args.value != null &&
    typeof message.args.value === "object"
  ) {
    const t = (message.args.value as Record<string, unknown>).title;
    if (typeof t === "string") title = t;
  }
  return (
    <div className="lutin-chat__msg lutin-chat__msg--tool">
      <div className="lutin-chat__tool" data-state={message.state}>
        <div className="lutin-chat__tool-head lutin-chat__tool-head--static">
          <span
            className="lutin-chat__tool-dot"
            data-state={message.state}
            aria-hidden="true"
            title={message.state}
          />
          <span className="lutin-chat__tool-name">{message.name}</span>
          {title && <span className="lutin-chat__tool-summary">{title}</span>}
        </div>
      </div>
    </div>
  );
}

function PrinciplePanel({ checks }: { checks: Check[] }) {
  const fails = checks.filter((c): c is Extract<Check, { kind: "fail" }> => c.kind === "fail");
  const passed = checks.filter((c) => c.kind === "pass").length;
  const headline =
    fails.length > 0
      ? `${fails.length} blocking · ${checks.length - fails.length} other`
      : `${passed} principle${passed === 1 ? "" : "s"} pass`;

  return (
    <div className="lutin-reviewed__review">
      <div className="lutin-reviewed__review-head">{headline}</div>
      <div className="lutin-reviewed__chips">
        {checks.map((c, i) => (
          <span
            key={i}
            className={`lutin-reviewed__chip lutin-reviewed__chip--${c.kind}`}
            title={c.kind === "skipped" ? `trusted after ${c.streak} passes` : undefined}
          >
            <span className="lutin-reviewed__chip-icon" aria-hidden="true">
              {c.kind === "pass" ? (
                <PassIcon />
              ) : c.kind === "fail" ? (
                <FailIcon />
              ) : (
                <SkipIcon />
              )}
            </span>
            {c.principle}
          </span>
        ))}
      </div>
      {fails.map((c, i) => (
        <div key={i} className="lutin-reviewed__feedback">
          <span className="lutin-reviewed__feedback-principle">{c.principle}</span>
          {c.feedback}
        </div>
      ))}
    </div>
  );
}

function PassIcon() {
  return (
    <svg width="10" height="10" viewBox="0 0 10 10" fill="none" aria-hidden>
      <path
        d="M2 5.2 4.2 7.4 8 3"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function FailIcon() {
  return (
    <svg width="10" height="10" viewBox="0 0 10 10" fill="none" aria-hidden>
      <path
        d="M2.8 2.8l4.4 4.4M7.2 2.8 2.8 7.2"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
      />
    </svg>
  );
}

function SkipIcon() {
  return (
    <svg width="10" height="10" viewBox="0 0 10 10" fill="none" aria-hidden>
      <path
        d="M1.5 5h5M4.5 2.5 7 5 4.5 7.5"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path d="M8.5 2.5v5" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" />
    </svg>
  );
}
