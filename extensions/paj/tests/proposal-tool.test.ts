import assert from "node:assert/strict";
import test from "node:test";

import {
  PROPOSAL_PROMPT_GUIDELINES,
  PROPOSAL_TOOL_NAME,
  proposalToolResult,
  setProposalToolActive,
} from "../proposal-tool-state.ts";

test("proposal guidance covers bridge response constraints", () => {
  assert.ok(
    PROPOSAL_PROMPT_GUIDELINES.some((line) =>
      line.includes("visible Markdown"),
    ),
  );
  assert.ok(
    PROPOSAL_PROMPT_GUIDELINES.some(
      (line) =>
        line.includes("recommends, suggests, or shows") &&
        line.includes("concrete repository change"),
    ),
  );
  assert.ok(
    PROPOSAL_PROMPT_GUIDELINES.some((line) => line.includes("final action")),
  );
  assert.ok(
    PROPOSAL_PROMPT_GUIDELINES.some((line) => line.includes("mere risks")),
  );
  assert.ok(
    PROPOSAL_PROMPT_GUIDELINES.some((line) => line.includes("alongside")),
  );
});

test("proposal result submits the single array and terminates", () => {
  let submitted: unknown;
  const input = [{ title: "Title", description: "Description" }];
  const result = proposalToolResult((actions) => {
    submitted = actions;
    return [{ id: "generated", title: "Title", description: "Description" }];
  }, input);

  assert.equal(submitted, input);
  assert.equal(result.terminate, true);
  assert.deepEqual(result.details.actions, [
    { id: "generated", title: "Title", description: "Description" },
  ]);
});

test("dynamic activation preserves unrelated active-tool changes", () => {
  let active = ["read"];
  const api = {
    getActiveTools: () => [...active],
    setActiveTools(names: string[]) {
      active = names;
    },
  };

  setProposalToolActive(api, true);
  assert.deepEqual(active, ["read", PROPOSAL_TOOL_NAME]);
  active = ["bash", PROPOSAL_TOOL_NAME, "unrelated"];
  setProposalToolActive(api, false);
  assert.deepEqual(active, ["bash", "unrelated"]);
  setProposalToolActive(api, false);
  assert.deepEqual(active, ["bash", "unrelated"]);
});
