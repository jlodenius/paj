import {
  DEFAULT_INSPECTION_HISTORY,
  MAX_INSPECTION_SNAPSHOT_BYTES,
  MAX_INSPECTION_HISTORY,
  MAX_INSPECTION_TEXT_BYTES,
  type InspectionSession,
  type InspectionSnapshot,
} from "./inspection-types.ts";

type RecordValue = Record<string, unknown>;
type HistoryItem = InspectionSnapshot["history"][number];
type Projection = { text: string; truncated: boolean };
const encoder = new TextEncoder();

function record(value: unknown): RecordValue | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as RecordValue
    : undefined;
}

function bounded(value: string, budget = MAX_INSPECTION_TEXT_BYTES): Projection {
  let text = "";
  let bytes = 2;
  for (const character of value) {
    const size = encoder.encode(JSON.stringify(character)).length - 2;
    if (bytes + size > budget) return { text, truncated: true };
    text += character;
    bytes += size;
  }
  return { text, truncated: false };
}

function visibleText(content: unknown): Projection {
  if (typeof content === "string") return bounded(content);
  let text = "";
  let truncated = false;
  if (!Array.isArray(content)) return { text, truncated };
  for (const block of content) {
    const item = record(block);
    const next = item?.type === "text" && typeof item.text === "string"
      ? item.text
      : item?.type === "image" ? "[image]" : "";
    if (!next) continue;
    const projected = bounded(text + next);
    text = projected.text;
    truncated ||= projected.truncated;
    if (truncated) break;
  }
  return { text, truncated };
}

function iso(value: unknown): string | undefined {
  if (typeof value !== "number" && typeof value !== "string") return undefined;
  const date = new Date(value);
  return Number.isFinite(date.getTime()) ? date.toISOString() : undefined;
}

function toolName(value: unknown): string {
  return typeof value === "string" && /^[\w.:-]{1,128}$/.test(value) ? value : "unknown";
}

function project(entry: unknown, index: number): HistoryItem | undefined {
  const item = record(entry);
  if (!item) return undefined;
  const message = item.type === "message" ? record(item.message) : undefined;
  let role: HistoryItem["role"];
  let projected: Projection;
  if (item.type === "custom_message" && item.display === true) {
    role = "user";
    projected = visibleText(item.content);
  } else if (message?.role === "user" || message?.role === "assistant") {
    role = message.role;
    projected = visibleText(message.content);
  } else if (message?.role === "toolResult") {
    role = "tool";
    projected = { text: `${toolName(message.toolName)}: ${message.isError === true ? "failed" : "completed"}`, truncated: false };
  } else {
    return undefined;
  }
  if (!projected.text && !projected.truncated) return undefined;
  const id = bounded(typeof item.id === "string" ? item.id : `branch-${index}`, 256);
  const timestamp = iso(message?.timestamp ?? item.timestamp);
  return {
    id: id.text,
    role,
    text: projected.text,
    ...(projected.truncated || id.truncated ? { truncated: true } : {}),
    ...(timestamp ? { timestamp } : {}),
  };
}

export function createInspectionView({ now = Date.now }: { now?: () => number } = {}) {
  let live: (Projection & {
    streaming: boolean;
    timestamp?: string;
    id?: string;
    previousAssistantId?: string;
    branchObserved: boolean;
  }) | null = null;
  let lastAssistantId: string | undefined;
  let branchObserved = false;
  let lastActivityAt: string | null = null;
  let tools = new Map<string, InspectionSnapshot["activeTools"][number]>();
  let activityTruncated = false;

  function clearTransient() {
    live = null;
    lastAssistantId = undefined;
    branchObserved = false;
    tools.clear();
    activityTruncated = false;
  }

  return {
    reset(): void {
      clearTransient();
      lastActivityAt = null;
    },
    observe(event: { type: string; [key: string]: unknown }): void {
      if (event.type === "session_tree" || event.type === "session_compact") {
        clearTransient();
        lastActivityAt = null;
        return;
      }
      if (event.type === "agent_settled") {
        clearTransient();
        lastActivityAt = iso(now()) ?? lastActivityAt;
        return;
      }
      if (!["agent_start", "message_start", "message_update", "message_end", "tool_execution_start", "tool_execution_update", "tool_execution_end"].includes(event.type)) return;
      lastActivityAt = iso(now()) ?? lastActivityAt;
      if (event.type.startsWith("message_")) {
        const message = record(event.message);
        if (message?.role !== "assistant") return;
        const projection = visibleText(message.content);
        live = {
          ...projection,
          streaming: event.type !== "message_end",
          previousAssistantId: lastAssistantId,
          branchObserved,
          ...(iso(message.timestamp) ? { timestamp: iso(message.timestamp) } : {}),
          ...(typeof message.id === "string" ? { id: bounded(message.id, 256).text } : {}),
        };
      } else if (event.type.startsWith("tool_execution_")) {
        if (typeof event.toolCallId !== "string") return;
        const id = bounded(event.toolCallId, 256);
        activityTruncated ||= id.truncated;
        if (event.type === "tool_execution_end") {
          tools.delete(id.text);
        } else if (event.type === "tool_execution_start") {
          tools.set(id.text, {
            toolCallId: id.text,
            toolName: toolName(event.toolName),
            startedAt: lastActivityAt ?? "1970-01-01T00:00:00.000Z",
          });
          if (tools.size > 32) {
            tools.delete(tools.keys().next().value!);
            activityTruncated = true;
          }
        }
      }
    },
    snapshot(identity: InspectionSession, ctx: {
      isIdle(): boolean;
      sessionManager: { getSessionId(): string; getBranch(): readonly unknown[] };
    }, limit = DEFAULT_INSPECTION_HISTORY): InspectionSnapshot {
      if (!Number.isInteger(limit) || limit < 1 || limit > MAX_INSPECTION_HISTORY) {
        throw new Error(`Inspection limit must be an integer from 1 to ${MAX_INSPECTION_HISTORY}`);
      }
      const sessionId = ctx.sessionManager.getSessionId();
      if (!identity.piSessionId || identity.piSessionId !== sessionId) {
        throw new Error("Inspection session identity changed");
      }
      for (const value of [identity.id, identity.piSessionId, identity.role, identity.name, identity.projectRoot]) {
        if (value !== undefined && (typeof value !== "string" || bounded(value, 2048).truncated)) {
          throw new Error("Invalid inspection identity");
        }
      }
      const branch = ctx.sessionManager.getBranch();
      const history: HistoryItem[] = [];
      let truncated = activityTruncated;
      let persisted = false;
      let latestAssistant: HistoryItem | undefined;
      for (let i = branch.length - 1; i >= 0; i--) {
        const item = project(branch[i], i);
        if (!item) continue;
        if (item.role === "assistant" && !latestAssistant) {
          latestAssistant = item;
          if (live && !live.streaming) {
            persisted = live.id ? live.id === item.id : live.text === item.text && (
              live.timestamp ? live.timestamp === item.timestamp
                : live.branchObserved && live.previousAssistantId !== item.id
            );
          }
        }
        if (history.length < limit) history.unshift(item);
        else {
          truncated = true;
          break;
        }
      }
      lastAssistantId = latestAssistant?.id;
      branchObserved = true;
      if (persisted) live = null;
      const result: InspectionSnapshot = {
        version: 1,
        source: "live-pi",
        identity: {
          id: identity.id,
          pid: identity.pid,
          piSessionId: sessionId,
          role: identity.role,
          ...(identity.name !== undefined ? { name: identity.name } : {}),
          ...(identity.projectRoot !== undefined ? { projectRoot: identity.projectRoot } : {}),
        },
        observedAt: iso(now()) ?? "1970-01-01T00:00:00.000Z",
        status: ctx.isIdle() ? "idle" : "busy",
        lastActivityAt,
        history,
        liveAssistant: live ? { text: live.text, streaming: live.streaming, ...(live.truncated ? { truncated: true } : {}) } : null,
        activeTools: Array.from(tools.values(), (tool) => ({ ...tool })),
        truncated: truncated || history.some((item) => item.truncated) || Boolean(live?.truncated),
        limitations: ["Only the active branch and visible text are included; reasoning, tool arguments and results are omitted."],
      };
      while (encoder.encode(JSON.stringify(result)).length >= MAX_INSPECTION_SNAPSHOT_BYTES) {
        result.truncated = true;
        if (result.history.length) result.history.shift();
        else if (result.activeTools.length) result.activeTools.shift();
        else throw new Error("Inspection identity exceeds snapshot size limit");
      }
      return result;
    },
  };
}
