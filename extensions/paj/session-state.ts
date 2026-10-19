export const MODEL_OVERRIDE_ENTRY = "paj-subagent-model";
export const REGISTRATION_ENTRY = "paj-registration";

function object(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

export function restoreModelOverride(
  entries: readonly unknown[],
): string | undefined {
  let model: string | undefined;
  for (const value of entries) {
    const entry = object(value);
    if (entry?.type !== "custom" || entry.customType !== MODEL_OVERRIDE_ENTRY)
      continue;
    const data = object(entry.data);
    if (data?.model === null) model = undefined;
    else if (typeof data?.model === "string") model = data.model;
  }
  return model;
}

export function restoreRegistrationId(
  entries: readonly unknown[],
): string | undefined {
  let id: string | undefined;
  for (const value of entries) {
    const entry = object(value);
    if (entry?.type !== "custom" || entry.customType !== REGISTRATION_ENTRY)
      continue;
    const data = object(entry.data);
    if (typeof data?.id === "string") id = data.id;
  }
  return id;
}

export function restoreDeliveredIds(entries: readonly unknown[]): Set<string> {
  const ids = new Set<string>();
  for (const value of entries) {
    const entry = object(value);
    if (!entry) continue;
    const data =
      entry.type === "custom_message" && entry.customType === "paj-messages"
        ? object(entry.details)
        : entry.type === "custom" && entry.customType === "paj-stale-messages"
          ? object(entry.data)
          : undefined;
    if (!Array.isArray(data?.messageIds)) continue;
    for (const id of data.messageIds) if (typeof id === "string") ids.add(id);
  }
  return ids;
}
