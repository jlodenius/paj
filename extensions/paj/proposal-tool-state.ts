import type { ProposalAction } from "./bridge.ts";

export const PROPOSAL_TOOL_NAME = "paj_propose_changes";

export const PROPOSAL_PROMPT_GUIDELINES = [
  "In every read-only bridge turn, call paj_propose_changes exactly once as the final action, after all visible Markdown and other tool use.",
  "Pass every concrete unimplemented repository change in one paj_propose_changes array; pass an empty array when the response contains none.",
  "Treat proposed code, replacement wording, refactor examples, and cleaner or better alternatives as recommendations, not mere explanations.",
  "In bridge turns, visible Markdown must explicitly state every unimplemented recommendation that paj_propose_changes includes.",
  "Do not include mere risks, explanations, or already implemented changes in paj_propose_changes.",
  "Do not call paj_propose_changes alongside any other tool calls.",
];

export interface ActiveToolAPI {
  getActiveTools(): string[];
  setActiveTools(names: string[]): void;
}

export function setProposalToolActive(
  pi: ActiveToolAPI,
  active: boolean,
): void {
  const current = pi.getActiveTools();
  if (active) {
    if (!current.includes(PROPOSAL_TOOL_NAME)) {
      pi.setActiveTools([...current, PROPOSAL_TOOL_NAME]);
    }
  } else if (current.includes(PROPOSAL_TOOL_NAME)) {
    pi.setActiveTools(current.filter((name) => name !== PROPOSAL_TOOL_NAME));
  }
}

export function proposalToolResult(
  submit: (actions: unknown) => ProposalAction[],
  input: unknown,
) {
  const actions = submit(input);
  return {
    content: [
      {
        type: "text" as const,
        text: `Attached ${actions.length} proposed change${actions.length === 1 ? "" : "s"}`,
      },
    ],
    details: { actions },
    terminate: true,
  };
}
