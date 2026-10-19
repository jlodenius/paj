import assert from "node:assert/strict";
import { access, readFile, stat } from "node:fs/promises";
import test from "node:test";

import { launchSubagent, withTaskFile } from "../subagent-launch.ts";

const task = "literal $HOME; $(touch /tmp/must-not-run)\nsecond line";

test("spawn passes an exact model and a private task file, never shell text", async () => {
  let taskPath = "";
  const child = await launchSubagent(
    async (command, args, options) => {
      assert.equal(command, "/skills/paj-subagents/scripts/tmux-agent");
      assert.equal(options.cwd, "/parent");
      assert.equal(
        args[args.indexOf("--model") + 1],
        "openai-codex/gpt-6.1-sol:xhigh",
      );
      assert.ok(args.includes("--json"));
      assert.ok(!args.includes(task));
      assert.equal(args[args.indexOf("--project") + 1], "example");
      taskPath = args[args.indexOf("--task-file") + 1];
      assert.equal(await readFile(taskPath, "utf8"), task);
      assert.equal((await stat(taskPath)).mode & 0o777, 0o600);
      return {
        code: 0,
        stdout: JSON.stringify({ spawnId: "spawn", tmuxName: "paj-child" }),
        stderr: "",
      };
    },
    "/skills/paj-subagents/scripts/tmux-agent",
    "/parent",
    { task, project: "example" },
    "openai-codex/gpt-6.1-sol:xhigh",
  );
  assert.equal(
    child.attachCommand,
    "TMUX= tmux -L paj attach-session -t =paj-child",
  );
  await assert.rejects(access(taskPath));
});

test("private files are removed on failure and empty tasks never launch", async () => {
  let taskPath = "";
  await assert.rejects(
    withTaskFile(task, async (path) => {
      taskPath = path;
      throw new Error("failed");
    }),
    /failed/,
  );
  await assert.rejects(access(taskPath));
  await assert.rejects(
    withTaskFile(" \n", async () => assert.fail("must not run")),
    /empty/,
  );
});
