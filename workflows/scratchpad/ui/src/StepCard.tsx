import { useState } from "react";
import type { Plan, Step, StepStatus, Verdict } from "./types";

type Props = {
  index: number;
  step: Step;
  forceExpanded?: boolean;
};

export function StepCard({ index, step, forceExpanded }: Props) {
  const isLive = step.status.stage !== "done";
  const [expanded, setExpanded] = useState(isLive || !!forceExpanded);

  const plan = "plan" in step.status ? step.status.plan : undefined;
  const summary = step.status.stage === "done" ? step.status.summary : undefined;

  return (
    <div
      className={`step ${expanded ? "expanded" : "collapsed"}`}
      onClick={!expanded ? () => setExpanded(true) : undefined}
    >
      <div className="step-header">
        <div className="step-title">
          <span className="step-num">#{index + 1}</span>
          {plan && <span className="step-tool">{plan.tool}</span>}
          {plan && <span className="step-goal">{plan.goal}</span>}
        </div>
        <StagePill status={step.status} />
        {expanded && !isLive && (
          <button
            className="stage-pill"
            onClick={(e) => {
              e.stopPropagation();
              setExpanded(false);
            }}
          >
            collapse
          </button>
        )}
      </div>

      {expanded ? (
        <div className="step-body">
          {plan && <PlanSection plan={plan} />}
          {step.iterations.length > 0 && <IterationsSection step={step} />}
          {step.persistent_failure && (
            <div className="persistent-failure">
              Agent has tried {step.persistent_failure.attempts} times and is still failing must-have
              <code> {step.persistent_failure.principle}</code>. Watching.
            </div>
          )}
          {step.status.stage === "execute" && (
            <div className="section">
              <h4>Execute</h4>
              <div className="args-block">running tool…</div>
            </div>
          )}
          {(step.status.stage === "summarize" || step.status.stage === "done") &&
            step.status.output && (
              <div className="section">
                <h4>Output</h4>
                <div className="args-block">{step.status.output}</div>
              </div>
            )}
          {summary && (
            <div className="section">
              <h4>Summary</h4>
              <div className="summary-text">{summary}</div>
            </div>
          )}
        </div>
      ) : (
        <div className="collapsed-summary">{summary ?? "…"}</div>
      )}
    </div>
  );
}

function StagePill({ status }: { status: StepStatus }) {
  const labels: Record<StepStatus["stage"], string> = {
    plan: "Planning",
    iterate: "Iterating",
    execute: "Executing",
    summarize: "Summarizing",
    done: "Done",
  };
  const cls = status.stage === "done" ? "done" : "active";
  return <span className={`stage-pill ${cls}`}>{labels[status.stage]}</span>;
}

function PlanSection({ plan }: { plan: Plan }) {
  return (
    <div className="section">
      <h4>Plan</h4>
      <div className="kv">
        <div className="k">tool</div>
        <div className="v">
          <code>{plan.tool}</code>
        </div>
        <div className="k">goal</div>
        <div className="v">{plan.goal}</div>
        <div className="k">why this tool</div>
        <div className="v">{plan.why_this_tool}</div>
        {plan.considerations.length > 0 && (
          <>
            <div className="k">considerations</div>
            <div className="v">
              <ul>
                {plan.considerations.map((c, i) => (
                  <li key={i}>{c}</li>
                ))}
              </ul>
            </div>
          </>
        )}
        <div className="k">args</div>
        <div className="v">
          <pre className="args-block">{JSON.stringify(plan.args, null, 2)}</pre>
        </div>
      </div>
    </div>
  );
}

function IterationsSection({ step }: { step: Step }) {
  const liveReviewing =
    step.status.stage === "iterate" ? step.status.current_principle ?? null : null;
  const lastIdx = step.iterations.length - 1;
  return (
    <div className="section">
      <h4>Iterations</h4>
      {step.iterations.map((it, i) => {
        const showLive = liveReviewing && i === lastIdx;
        return (
          <div key={it.index} className="iteration">
            <div className="iteration-header">
              <span>iteration {it.index + 1}</span>
            </div>
            {it.principle_verdicts.map((pv, i) => (
              <VerdictRow key={i} principle={pv.principle} verdict={pv.verdict} />
            ))}
            {showLive && (
              <div className="verdict-row">
                <span className="verdict-badge">reviewing…</span>
                <span className="principle">{liveReviewing}</span>
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

function VerdictRow({ principle, verdict }: { principle: string; verdict: Verdict }) {
  return (
    <div className="verdict-row">
      <div className="verdict-row-head">
        <span className={`verdict-badge ${verdict.kind}`}>{verdict.kind}</span>
        <span className="principle">{principle}</span>
      </div>
      {verdict.kind !== "pass" && (
        <div className="verdict-feedback">{verdict.feedback}</div>
      )}
    </div>
  );
}
