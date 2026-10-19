export interface TaskResultSummary {
  taskId: string;
  parentTaskId?: string;
  delivered: boolean;
}

export interface SpawnRecord {
  spawnId: string;
  parentPiSessionId: string;
  parentTaskId?: string;
  parentPid: number;
  childPiSessionId?: string;
  childPajSessionId?: string;
  name: string;
  tmuxName: string;
  cwd: string;
  projectRoot: string;
  task: string;
  taskId?: string;
  taskState?: "active" | "completed";
  requestedModel?: string;
  result?: TaskResultSummary;
  resultHistory?: TaskResultSummary[];
  assignment?: { delivered: boolean };
  createdAtMs: number;
}

export interface SubagentSession {
  id: string;
  status: string;
  model?: string;
}

export interface ListedSubagent extends SpawnRecord {
  status: "idle" | "busy" | "starting";
  attachCommand: string;
  model?: string;
}

export function combineSubagents(
  records: SpawnRecord[],
  sessions: SubagentSession[],
): ListedSubagent[] {
  const byId = new Map(sessions.map((session) => [session.id, session]));
  return records.map(
    ({
      result: _result,
      resultHistory: _history,
      assignment: _assignment,
      ...record
    }) => ({
      ...record,
      model: record.childPajSessionId
        ? byId.get(record.childPajSessionId)?.model
        : undefined,
      status:
        (record.childPajSessionId
          ? byId.get(record.childPajSessionId)?.status
          : undefined) === "busy"
          ? "busy"
          : record.childPajSessionId && byId.has(record.childPajSessionId)
            ? "idle"
            : "starting",
      attachCommand: `TMUX= tmux -L paj attach-session -t =${record.tmuxName}`,
    }),
  );
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

export interface SubagentFormatStyle {
  name: (value: string) => string;
  status: (value: ListedSubagent["status"]) => string;
  label: (value: string) => string;
  value: (value: string) => string;
  command: (value: string) => string;
}

const plainStyle: SubagentFormatStyle = {
  name: (value) => value,
  status: (value) => `[${value}]`,
  label: (value) => value,
  value: (value) => value,
  command: (value) => value,
};

export function formatSubagents(
  subagents: ListedSubagent[],
  style: SubagentFormatStyle = plainStyle,
): string {
  return subagents
    .map((child) => {
      const row = (label: string, value: string) =>
        `  ${style.label(label.padEnd(8))}${value}`;
      const attach = child.attachCommand.replace(/^TMUX= tmux /, "tmux ");
      return [
        `${style.name(child.name)} ${style.status(child.status)}`,
        row(
          "model",
          style.value(
            child.model ??
              (child.requestedModel
                ? `${child.requestedModel} (unconfirmed)`
                : "unknown"),
          ),
        ),
        row("task", style.value(child.taskState ?? "active")),
        row("id", style.value(child.spawnId)),
        row("cwd", style.value(child.cwd)),
        row("prompt", style.value(child.task.split("\n", 1)[0])),
        row("tmux", style.value(child.tmuxName)),
        row("attach", style.command(attach)),
      ].join("\n");
    })
    .join("\n\n");
}
