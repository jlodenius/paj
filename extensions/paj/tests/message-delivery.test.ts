import assert from "node:assert/strict";
import test from "node:test";

import {
  deliverPendingMessages,
  type PendingMessage,
} from "../message-delivery.ts";

const messages: PendingMessage[] = Array.from({ length: 12 }, (_, index) => ({
  id: String(index),
  kind: "question",
  taskId: "task",
}));

function harness(idle = true) {
  const deliveries: Array<{ ids: string[]; disposition: string }> = [];
  const acknowledged: string[] = [];
  return {
    deliveries,
    acknowledged,
    deliveredIds: new Set<string>(),
    deliver: (batch: PendingMessage[], disposition: string) => {
      deliveries.push({ ids: batch.map((message) => message.id), disposition });
    },
    acknowledge: async (message: PendingMessage) => {
      acknowledged.push(message.id);
    },
    isIdle: () => idle,
  };
}

test("busy recipients keep the entire inbox pending without creating Pi follow-ups", async () => {
  const h = harness(false);
  await deliverPendingMessages(
    messages,
    h.deliveredIds,
    h.deliver,
    h.acknowledge,
    h.isIdle,
  );
  assert.deepEqual(h.deliveries, []);
  assert.deepEqual(h.acknowledged, []);
});

test("an idle recipient gets one batch rather than twelve separate turns", async () => {
  const h = harness();
  await deliverPendingMessages(
    messages,
    h.deliveredIds,
    h.deliver,
    h.acknowledge,
    h.isIdle,
  );
  assert.deepEqual(h.deliveries, [
    { ids: messages.map((message) => message.id), disposition: "actionable" },
  ]);
  assert.equal(h.acknowledged.length, 12);
});

test("a completed task archives late messages instead of waking the child", async () => {
  const h = harness();
  await deliverPendingMessages(
    messages,
    h.deliveredIds,
    h.deliver,
    h.acknowledge,
    h.isIdle,
    {
      taskId: "task",
      taskState: "completed",
    },
  );
  assert.equal(h.deliveries[0].disposition, "stale");
  assert.equal(h.acknowledged.length, 12);
});

test("a new explicit assignment cannot revive messages for its predecessor", async () => {
  const h = harness();
  await deliverPendingMessages(
    [
      { id: "old", kind: "question", taskId: "old-task" },
      { id: "new", kind: "request", taskId: "new-task" },
      { id: "legacy", kind: "request" },
    ],
    h.deliveredIds,
    h.deliver,
    h.acknowledge,
    h.isIdle,
    {
      taskId: "new-task",
      taskState: "active",
    },
  );
  assert.deepEqual(h.deliveries, [
    { ids: ["old", "legacy"], disposition: "stale" },
    { ids: ["new"], disposition: "actionable" },
  ]);
});

test("sibling replies use the recipient assignment, not the conversation task, for delivery", async () => {
  const h = harness();
  await deliverPendingMessages(
    [
      {
        id: "answer",
        kind: "answer" as const,
        taskId: "sibling-task",
        recipientTaskId: "my-task",
      },
      {
        id: "late",
        kind: "answer" as const,
        taskId: "my-task",
        recipientTaskId: "previous-task",
      },
    ],
    h.deliveredIds,
    h.deliver,
    h.acknowledge,
    h.isIdle,
    {
      taskId: "my-task",
      taskState: "active",
    },
  );
  assert.deepEqual(h.deliveries, [
    { ids: ["late"], disposition: "stale" },
    { ids: ["answer"], disposition: "actionable" },
  ]);
});

test("updates do not wake the recipient, while final results wake the parent once", async () => {
  const h = harness();
  await deliverPendingMessages(
    [
      { id: "update", kind: "update", taskId: "task" },
      { id: "result", kind: "result", taskId: "task" },
    ],
    h.deliveredIds,
    h.deliver,
    h.acknowledge,
    h.isIdle,
  );
  assert.deepEqual(h.deliveries, [
    { ids: ["update"], disposition: "update" },
    { ids: ["result"], disposition: "actionable" },
  ]);
});

test("an ACK failure retries without delivering any batch twice", async () => {
  const h = harness();
  let fail = true;
  const acknowledge = async (message: PendingMessage) => {
    if (fail && message.id === "0") throw new Error("ACK unavailable");
    await h.acknowledge(message);
  };
  await assert.rejects(
    deliverPendingMessages(
      messages,
      h.deliveredIds,
      h.deliver,
      acknowledge,
      h.isIdle,
    ),
  );
  assert.deepEqual([...h.deliveredIds], ["0"]);
  fail = false;
  await deliverPendingMessages(
    [messages[0]],
    h.deliveredIds,
    h.deliver,
    acknowledge,
    h.isIdle,
  );
  assert.equal(h.deliveries.length, 1);
  assert.equal(h.deliveredIds.size, 0);
});

test("failed injection leaves messages eligible for later delivery", async () => {
  const h = harness();
  await assert.rejects(
    deliverPendingMessages(
      messages,
      h.deliveredIds,
      () => {
        throw new Error("injection failed");
      },
      h.acknowledge,
      h.isIdle,
    ),
  );
  assert.equal(h.deliveredIds.size, 0);
  assert.deepEqual(h.acknowledged, []);
});

test("restored delivery receipts prevent reinjection after a reload", async () => {
  const h = harness();
  h.deliveredIds.add("0");
  await deliverPendingMessages(
    [messages[0]],
    h.deliveredIds,
    h.deliver,
    h.acknowledge,
    h.isIdle,
  );
  assert.deepEqual(h.deliveries, []);
  assert.deepEqual(h.acknowledged, ["0"]);
});
