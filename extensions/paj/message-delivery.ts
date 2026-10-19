export type MessageKind =
  | "request"
  | "question"
  | "answer"
  | "update"
  | "result";
export type MessageDisposition = "actionable" | "update" | "stale";

export interface PendingMessage {
  id: string;
  kind?: MessageKind;
  taskId?: string;
  recipientTaskId?: string;
}

export interface RecipientTask {
  taskId: string;
  taskState: "active" | "completed";
  childTaskIds?: readonly string[];
}

export async function deliverPendingMessages<T extends PendingMessage>(
  messages: T[],
  deliveredIds: Set<string>,
  deliver: (messages: T[], disposition: MessageDisposition) => void,
  acknowledge: (message: T) => Promise<void>,
  isIdle: () => boolean,
  task?: RecipientTask,
): Promise<void> {
  if (!isIdle()) return;

  const pendingIds = new Set(messages.map((message) => message.id));
  for (const id of deliveredIds) {
    if (!pendingIds.has(id)) deliveredIds.delete(id);
  }

  const batches: Record<MessageDisposition, T[]> = {
    stale: [],
    update: [],
    actionable: [],
  };
  for (const message of messages) {
    if (deliveredIds.has(message.id)) continue;
    const belongsToTask =
      !task ||
      (message.recipientTaskId !== undefined
        ? message.recipientTaskId === task.taskId
        : message.taskId === task.taskId ||
          (message.taskId !== undefined &&
            task.childTaskIds?.includes(message.taskId)));
    const disposition =
      task && (task.taskState === "completed" || !belongsToTask)
        ? "stale"
        : message.kind === "update"
          ? "update"
          : "actionable";
    batches[disposition].push(message);
  }
  for (const disposition of ["stale", "update", "actionable"] as const) {
    const batch = batches[disposition];
    if (!batch.length) continue;
    deliver(batch, disposition);
    for (const message of batch) deliveredIds.add(message.id);
  }

  let firstError: unknown;
  for (const message of messages) {
    try {
      await acknowledge(message);
      deliveredIds.delete(message.id);
    } catch (error) {
      firstError ??= error;
    }
  }
  if (firstError !== undefined) throw firstError;
}
