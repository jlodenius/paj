import assert from "node:assert/strict";
import test from "node:test";

import {
  MODEL_OVERRIDE_ENTRY,
  restoreDeliveredIds,
  restoreModelOverride,
} from "../session-state.ts";

const override = (model: string | null) => ({
  type: "custom",
  customType: MODEL_OVERRIDE_ENTRY,
  data: { model },
});

test("the latest override is restored on the current branch and auto clears it", () => {
  assert.equal(restoreModelOverride([]), undefined);
  assert.equal(restoreModelOverride([override("one"), override("two")]), "two");
  assert.equal(
    restoreModelOverride([override("one"), override(null)]),
    undefined,
  );
  assert.equal(
    restoreModelOverride([null, {}, override("one"), { type: "custom" }]),
    "one",
  );
});

test("delivery receipts are restored for injected and archived messages only", () => {
  assert.deepEqual(
    [
      ...restoreDeliveredIds([
        {
          type: "custom_message",
          customType: "paj-messages",
          details: { messageIds: ["one", "two"] },
        },
        {
          type: "custom",
          customType: "paj-stale-messages",
          data: { messageIds: ["three"] },
        },
        {
          type: "custom",
          customType: "unrelated",
          data: { messageIds: ["four"] },
        },
      ]),
    ],
    ["one", "two", "three"],
  );
});
