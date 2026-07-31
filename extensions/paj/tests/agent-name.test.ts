import assert from "node:assert/strict";
import test from "node:test";

import { renamePajSession } from "../agent-name.ts";

test("renaming invokes the Paj CLI for the active session", async () => {
  const calls: Array<{ args: string[]; cwd: string }> = [];

  const session = await renamePajSession(
    async (args, cwd) => {
      calls.push({ args, cwd });
      return JSON.stringify({ id: "session-id", name: "reviewer" });
    },
    "session-id",
    "reviewer",
    "/project",
  );

  assert.deepEqual(calls, [
    {
      args: [
        "--json",
        "session",
        "rename",
        "session-id",
        "reviewer",
      ],
      cwd: "/project",
    },
  ]);
  assert.deepEqual(session, { id: "session-id", name: "reviewer" });
});
