import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

import type { ProposalAction } from "./bridge.ts";
import {
  PROPOSAL_PROMPT_GUIDELINES,
  PROPOSAL_TOOL_NAME,
  proposalToolResult,
  setProposalToolActive,
} from "./proposal-tool-state.ts";

const proposalParameters = Type.Object({
  actions: Type.Array(
    Type.Object(
      {
        title: Type.String({ description: "Short action title" }),
        description: Type.String({
          description: "Complete description of the proposed unimplemented change",
        }),
      },
      { additionalProperties: false },
    ),
    {
      description: "All proposed changes for this bridge response",
      minItems: 0,
      maxItems: 20,
    },
  ),
});

export function registerProposalTool(
  pi: ExtensionAPI,
  submit: (actions: unknown) => ProposalAction[],
): (active: boolean) => void {
  pi.registerTool({
    name: PROPOSAL_TOOL_NAME,
    label: "Propose changes",
    description:
      "Attach structured actions for concrete changes recommended but not implemented in the current bridge response.",
    promptSnippet:
      "Attach every concrete unimplemented repository change to the bridge response",
    promptGuidelines: PROPOSAL_PROMPT_GUIDELINES,
    parameters: proposalParameters,
    async execute(_toolCallId, params) {
      return proposalToolResult(submit, params.actions);
    },
  });

  return (active: boolean) => setProposalToolActive(pi, active);
}
