export interface SpawnRecord {
  spawnId: string;
  parentPiSessionId: string;
  parentPid: number;
  childPiSessionId?: string;
  childPajSessionId?: string;
  name: string;
  tmuxName: string;
  cwd: string;
  projectRoot: string;
  task: string;
  createdAtMs: number;
}

export interface SubagentSession {
  id: string;
  status: string;
}

export interface ListedSubagent extends SpawnRecord {
  status: "idle" | "busy" | "starting";
  attachCommand: string;
}

export function combineSubagents(
  records: SpawnRecord[],
  sessions: SubagentSession[],
): ListedSubagent[] {
  const byId = new Map(sessions.map((session) => [session.id, session]));
  return records.map((record) => ({
    ...record,
    status:
      (record.childPajSessionId
        ? byId.get(record.childPajSessionId)?.status
        : undefined) === "busy"
        ? "busy"
        : record.childPajSessionId && byId.has(record.childPajSessionId)
          ? "idle"
          : "starting",
    attachCommand: `TMUX= tmux -L paj attach-session -t =${record.tmuxName}`,
  }));
}

export function shouldStopSubagents(reason: string): boolean {
  return reason !== "reload";
}

export async function cleanupSubagents(
  subagents: ListedSubagent[],
  kill: (subagent: ListedSubagent) => Promise<void>,
  remove: (subagent: ListedSubagent) => Promise<void>,
): Promise<void> {
  const errors: unknown[] = [];
  for (const subagent of subagents) {
    try {
      await kill(subagent);
    } catch (error) {
      errors.push(error);
      continue;
    }
    try {
      await remove(subagent);
    } catch (error) {
      errors.push(error);
    }
  }
  if (errors.length) {
    throw new AggregateError(
      errors,
      `Failed to clean up ${errors.length} subagent operation(s)`,
    );
  }
}

export function formatSubagents(subagents: ListedSubagent[]): string {
  return subagents
    .map(
      (child) =>
        `${child.name} [${child.status}] ${child.projectRoot} — ${child.task.split("\n", 1)[0]}\n  spawn: ${child.spawnId}  tmux: ${child.tmuxName}\n  ${child.attachCommand}`,
    )
    .join("\n");
}
