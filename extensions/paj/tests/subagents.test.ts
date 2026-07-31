import assert from "node:assert/strict";
import test from "node:test";

import {
  cleanupSubagents,
  combineSubagents,
  formatSubagents,
  shouldStopSubagents,
  type SpawnRecord,
} from "../subagents.ts";

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
  assert.equal(
    formatSubagents([child]),
    [
      "agent-child [busy]",
      "  cwd     /project/src",
      "  prompt  Run tests",
      "  id      spawn-id",
      "  tmux    paj-child",
      "  attach  tmux -L paj attach-session -t =paj-child",
    ].join("\n"),
  );
  assert.doesNotMatch(formatSubagents([child]), /and report/);
});

test("subagent listing separates agents and styles labels independently", () => {
  const children = combineSubagents(
    [record, { ...record, spawnId: "spawn-two", name: "agent-two" }],
    [{ id: "child-paj", status: "busy" }],
  );
  const formatted = formatSubagents(children, {
    name: (value) => `<name>${value}</name>`,
    status: (value) => `<status>${value}</status>`,
    label: (value) => `<label>${value}</label>`,
    value: (value) => `<value>${value}</value>`,
    command: (value) => `<command>${value}</command>`,
  });

  assert.match(formatted, /<name>agent-child<\/name> <status>busy<\/status>/);
  assert.match(formatted, /<label>cwd     <\/label><value>\/project\/src<\/value>/);
  assert.match(formatted, /<label>prompt  <\/label><value>Run tests<\/value>/);
  assert.match(formatted, /<label>attach  <\/label><command>tmux -L paj attach-session/);
  assert.match(formatted, /<\/command>\n\n<name>agent-two<\/name>/);
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
