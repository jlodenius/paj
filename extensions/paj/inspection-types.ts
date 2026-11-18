export const MAX_INSPECTION_BYTES = 65_536;
export const MAX_INSPECTION_SNAPSHOT_BYTES = MAX_INSPECTION_BYTES - 2_048;
export const MAX_INSPECTION_HISTORY = 50;
export const DEFAULT_INSPECTION_HISTORY = 20;
export const MAX_INSPECTION_TEXT_BYTES = 4_096;

export interface InspectionSession {
  id: string;
  pid: number;
  piSessionId?: string;
  role: string;
  parentPiSessionId?: string | null;
  name?: string;
  projectRoot?: string;
  bridgeSocket?: string;
  captainInspection?: string;
  startedAtMs?: number;
  lastHeartbeatMs?: number;
  status?: string;
}

export interface InspectionCaller {
  id: string;
  pid: number;
  piSessionId: string;
}

export interface InspectionSpawn {
  spawnId: string;
  parentPiSessionId: string;
  parentPid: number;
  parentTaskId?: string;
  childPiSessionId?: string;
  childPajSessionId?: string;
  taskId?: string;
  taskState?: "active" | "completed";
}

export interface InspectionRegistry {
  listSessions(): Promise<InspectionSession[]>;
  listSpawns(): Promise<InspectionSpawn[]>;
}

export interface InspectionSnapshot {
  version: 1;
  source: "live-pi";
  identity: {
    id: string;
    pid: number;
    piSessionId: string;
    role: string;
    name?: string;
    projectRoot?: string;
  };
  observedAt: string;
  status: "idle" | "busy";
  lastActivityAt: string | null;
  history: {
    id: string;
    role: "user" | "assistant" | "tool";
    text: string;
    truncated?: boolean;
    timestamp?: string;
  }[];
  liveAssistant: { text: string; streaming: boolean; truncated?: boolean } | null;
  activeTools: { toolCallId: string; toolName: string; startedAt: string }[];
  truncated: boolean;
  limitations: string[];
}
