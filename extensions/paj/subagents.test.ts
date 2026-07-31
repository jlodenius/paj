import assert from "node:assert/strict";
import test from "node:test";

import {
  combineSubagents,
  formatSubagents,
  shouldStopSubagents,
  type SpawnRecord,
} from "./subagents.ts";

const record: SpawnRecord = {
  spawnId: "spawn-id",
  parentPiSessionId: "parent-pi",
  parentPid: 1,
  childPiSessionId: "child-pi",
  childPajSessionId: "child-paj",
  name: "agent-child",
  tmuxName: "paj-child",
  cwd: "/project/src",
  projectRoot: "/project",
  task: "Run tests\nand report",
  createdAtMs: 1,
};

test("subagent listing joins active status and emits a cross-server attach command", () => {
  const [child] = combineSubagents([record], [
    { id: "child-paj", status: "busy" },
  ]);

  assert.equal(child.status, "busy");
  assert.equal(
    child.attachCommand,
    "TMUX= tmux -L paj attach-session -t =paj-child",
  );
  assert.match(formatSubagents([child]), /Run tests/);
  assert.doesNotMatch(formatSubagents([child]), /and report/);
});

test("shutdown cleanup preserves children only for reload", () => {
  assert.equal(shouldStopSubagents("reload"), false);
  for (const reason of ["quit", "new", "resume", "fork"]) {
    assert.equal(shouldStopSubagents(reason), true);
  }
});

test("an unregistered child is shown as starting", () => {
  const [child] = combineSubagents(
    [{ ...record, childPajSessionId: undefined }],
    [],
  );

  assert.equal(child.status, "starting");
});
