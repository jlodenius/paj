import assert from "node:assert/strict";
import test from "node:test";

import { setPajSessionStatus } from "./session-status.ts";

test("setting status invokes the Paj CLI for the active session", async () => {
  const calls: Array<{ args: string[]; cwd: string }> = [];

  await setPajSessionStatus(
    async (args, cwd) => {
      calls.push({ args, cwd });
      return "";
    },
    "session-id",
    "busy",
    "/project",
  );

  assert.deepEqual(calls, [
    {
      args: ["session", "status", "session-id", "busy"],
      cwd: "/project",
    },
  ]);
});
