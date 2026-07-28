export interface PendingMessage {
  id: string;
}

export async function deliverPendingMessages<T extends PendingMessage>(
  messages: T[],
  deliveredIds: Set<string>,
  deliver: (message: T, immediately: boolean) => void,
  acknowledge: (message: T) => Promise<void>,
  canDeliverImmediately: boolean,
): Promise<void> {
  const pendingIds = new Set(messages.map((message) => message.id));
  for (const id of deliveredIds) {
    if (!pendingIds.has(id)) {
      deliveredIds.delete(id);
    }
  }

  let deliverImmediately = canDeliverImmediately;
  let firstError: unknown;
  for (const message of messages) {
    if (!deliveredIds.has(message.id)) {
      deliver(message, deliverImmediately);
      deliveredIds.add(message.id);
      deliverImmediately = false;
    }
    try {
      await acknowledge(message);
      deliveredIds.delete(message.id);
    } catch (error) {
      firstError ??= error;
    }
  }

  if (firstError !== undefined) {
    throw firstError;
  }
}
