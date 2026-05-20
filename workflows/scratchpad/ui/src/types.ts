export type Plan = {
  tool: string;
  goal: string;
  why_this_tool: string;
  considerations: string[];
  args: unknown;
};

export type FixEntry = {
  principle: string;
  feedback: string;
};

export type Verdict =
  | { kind: "pass" }
  | { kind: "fix"; feedback: string }
  | { kind: "rethink"; feedback: string };

export type IterationRecord = {
  index: number;
  args: unknown;
  principle_verdicts: Array<{ principle: string; verdict: Verdict }>;
};

export type StageKind = "plan" | "iterate" | "execute" | "summarize" | "done";

export type StepStatus =
  | { stage: "plan" }
  | { stage: "iterate"; plan: Plan; fix_log: FixEntry[]; current_principle?: string }
  | { stage: "execute"; plan: Plan }
  | { stage: "summarize"; plan: Plan; output: string }
  | { stage: "done"; plan: Plan; summary: string; output: string };

export type Step = {
  id: string;
  status: StepStatus;
  iterations: IterationRecord[];
  persistent_failure?: { principle: string; attempts: number };
};

export type UserTurn = {
  kind: "user";
  id: string;
  text: string;
};

export type AssistantTurn = {
  kind: "assistant";
  id: string;
  text: string;
};

export type StepTurn = {
  kind: "step";
  id: string;
  step: Step;
};

export type Turn = UserTurn | AssistantTurn | StepTurn;
