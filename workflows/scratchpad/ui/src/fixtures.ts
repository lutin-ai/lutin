import type { Turn } from "./types";

export const FIXTURES: Turn[] = [
  {
    kind: "user",
    id: "u-1",
    text: "Add a bounds check to parse_input so it rejects empty strings.",
  },
  {
    kind: "step",
    id: "s-1",
    step: {
      id: "s-1",
      status: {
        stage: "done",
        plan: {
          tool: "edit",
          goal: "Add an early return rejecting empty input to parse_input",
          why_this_tool: "Targeted modification to one function in one file",
          considerations: ["Preserve existing tests", "Match project's guard-clause style"],
          args: {
            path: "src/parse.rs",
            old_string: "fn parse_input(s: &str) -> Result<Value> {",
            new_string:
              'fn parse_input(s: &str) -> Result<Value> {\n    if s.is_empty() {\n        return Err(anyhow!("parse_input: empty input"));\n    }',
          },
        },
        summary:
          "Added an empty-string guard at the top of parse_input in src/parse.rs. The error message includes the function name for traceability, and the guard-clause style avoids nesting. Existing tests are untouched; the next caller of parse_input on an empty string will now see a clean error rather than a panic.",
        output: "applied; 3 lines added at line 8",
      },
      iterations: [
        {
          index: 0,
          args: { path: "src/parse.rs", old_string: "...", new_string: "...if empty..." },
          principle_verdicts: [
            { principle: "guard-clauses-over-nesting", verdict: { kind: "pass" } },
            {
              principle: "clear-error-messages-for-humans",
              verdict: {
                kind: "fix",
                feedback: "Error strings should reference the function name for traceability.",
              },
            },
          ],
        },
        {
          index: 1,
          args: { path: "src/parse.rs", old_string: "...", new_string: "...parse_input: empty..." },
          principle_verdicts: [
            { principle: "guard-clauses-over-nesting", verdict: { kind: "pass" } },
            { principle: "clear-error-messages-for-humans", verdict: { kind: "pass" } },
            { principle: "no-premature-abstraction", verdict: { kind: "pass" } },
          ],
        },
      ],
    },
  },
  {
    kind: "user",
    id: "u-2",
    text: "Now wire it through main.rs.",
  },
  {
    kind: "step",
    id: "s-2",
    step: {
      id: "s-2",
      status: {
        stage: "iterate",
        plan: {
          tool: "edit",
          goal: "Call the bounds-checking parse_input from main.rs",
          why_this_tool: "Single call site to insert; edit is right tool",
          considerations: ["Don't shadow the existing variable", "Propagate the error with ?"],
          args: { path: "src/main.rs", old_string: "let v = raw;", new_string: "let v = parse_input(raw)?;" },
        },
        fix_log: [
          {
            principle: "errors-as-values",
            feedback: "Don't unwrap — propagate with ?",
          },
        ],
        current_principle: "name-things-for-what-they-are",
      },
      iterations: [
        {
          index: 0,
          args: {},
          principle_verdicts: [
            { principle: "errors-as-values", verdict: { kind: "fix", feedback: "Don't unwrap — propagate with ?" } },
          ],
        },
      ],
    },
  },
];
