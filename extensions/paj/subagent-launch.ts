import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import type { SpawnRecord } from "./subagents.ts";

export interface SpawnParams {
  task: string;
  project?: string;
  cwd?: string;
  model?: string;
}

export type Execute = (
  command: string,
  args: string[],
  options: { cwd: string; timeout: number },
) => Promise<{ code: number; stdout: string; stderr: string }>;

export async function withTaskFile<T>(
  task: string,
  action: (path: string) => Promise<T>,
): Promise<T> {
  if (!task.trim()) throw new Error("Task cannot be empty");
  const directory = await mkdtemp(join(tmpdir(), "paj-task-"));
  try {
    const path = join(directory, "task");
    await writeFile(path, task, { mode: 0o600 });
    return await action(path);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}

export async function launchSubagent(
  execute: Execute,
  helper: string,
  parentCwd: string,
  params: SpawnParams,
  model: string,
): Promise<SpawnRecord & { attachCommand: string }> {
  return withTaskFile(params.task, async (path) => {
    const args = ["spawn", "--json", "--model", model, "--task-file", path];
    if (params.project) args.push("--project", params.project);
    if (params.cwd) args.push("--cwd", params.cwd);
    const result = await execute(helper, args, {
      cwd: parentCwd,
      timeout: 30_000,
    });
    if (result.code !== 0)
      throw new Error(result.stderr.trim() || "Subagent launch failed");
    const record = JSON.parse(result.stdout) as SpawnRecord;
    if (!record.spawnId || !record.tmuxName)
      throw new Error("Invalid subagent launch response");
    return {
      ...record,
      attachCommand: `TMUX= tmux -L paj attach-session -t =${record.tmuxName}`,
    };
  });
}
