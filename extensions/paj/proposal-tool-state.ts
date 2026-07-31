import type { ProposalAction } from "./bridge.ts";

export const PROPOSAL_TOOL_NAME = "paj_propose_changes";

export const PROPOSAL_PROMPT_GUIDELINES = [
  "In bridge turns, visible Markdown must explicitly state every unimplemented recommendation that paj_propose_changes includes.",
  "Call paj_propose_changes exactly once as the final action of a bridge turn, with every proposal in one array.",
  "Do not use paj_propose_changes for mere risks or explanations, or for changes already implemented in the response.",
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
