import assert from "node:assert/strict";
import test from "node:test";

import {
  cleanupSubagents,
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

test("cleanup attempts later children after a record removal fails", async () => {
  const children = combineSubagents(
    [record, { ...record, spawnId: "spawn-two", tmuxName: "paj-two" }],
    [],
  );
  const killed: string[] = [];
  const removed: string[] = [];

  await assert.rejects(
    cleanupSubagents(
      children,
      async (child) => {
        killed.push(child.spawnId);
      },
      async (child) => {
        removed.push(child.spawnId);
        if (child.spawnId === "spawn-id") throw new Error("remove failed");
      },
    ),
    AggregateError,
  );

  assert.deepEqual(killed, ["spawn-id", "spawn-two"]);
  assert.deepEqual(removed, ["spawn-id", "spawn-two"]);
});

test("an unregistered child is shown as starting", () => {
  const [child] = combineSubagents(
    [{ ...record, childPajSessionId: undefined }],
    [],
  );

  assert.equal(child.status, "starting");
});
