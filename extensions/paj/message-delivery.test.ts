import assert from "node:assert/strict";
import test from "node:test";

import { deliverPendingMessages } from "./message-delivery.ts";

interface Message {
  id: string;
  text: string;
}

const messages: Message[] = [
  { id: "one", text: "first" },
  { id: "two", text: "second" },
];

test("an ACK failure is retried without delivering the message twice", async () => {
  const deliveredIds = new Set<string>();
  const deliveries: string[] = [];
  let attempts = 0;
  const acknowledge = async (message: Message) => {
    attempts++;
    if (message.id === "one" && attempts === 1) {
      throw new Error("temporary ACK failure");
    }
  };

  await assert.rejects(
    deliverPendingMessages(
      messages,
      deliveredIds,
      (message) => deliveries.push(message.id),
      acknowledge,
      true,
    ),
    /temporary ACK failure/,
  );
  assert.deepEqual(deliveries, ["one", "two"]);
  assert.deepEqual([...deliveredIds], ["one"]);

  await deliverPendingMessages(
    [messages[0]],
    deliveredIds,
    (message) => deliveries.push(message.id),
    acknowledge,
    false,
  );
  assert.deepEqual(deliveries, ["one", "two"]);
  assert.deepEqual([...deliveredIds], []);
});

test("delivery failures remain eligible for a later retry", async () => {
  const deliveredIds = new Set<string>();
  let attempts = 0;

  await assert.rejects(
    deliverPendingMessages(
      [messages[0]],
      deliveredIds,
      () => {
        attempts++;
        throw new Error("injection failed");
      },
      async () => undefined,
      true,
    ),
    /injection failed/,
  );
  assert.equal(attempts, 1);
  assert.deepEqual([...deliveredIds], []);
});

test("stale delivered IDs are pruned and only the first new message is immediate", async () => {
  const deliveredIds = new Set(["gone"]);
  const deliveries: Array<[string, boolean]> = [];

  await deliverPendingMessages(
    messages,
    deliveredIds,
    (message, immediately) => deliveries.push([message.id, immediately]),
    async () => undefined,
    true,
  );

  assert.deepEqual(deliveries, [
    ["one", true],
    ["two", false],
  ]);
  assert.deepEqual([...deliveredIds], []);
});
