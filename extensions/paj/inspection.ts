import { randomUUID } from "node:crypto";
import { lstat, chmod, unlink, realpath, link } from "node:fs/promises";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { createConnection, createServer, type Server, type Socket } from "node:net";
import {
  DEFAULT_INSPECTION_HISTORY, MAX_INSPECTION_BYTES, MAX_INSPECTION_HISTORY,
  MAX_INSPECTION_TEXT_BYTES, MAX_INSPECTION_SNAPSHOT_BYTES,
  type InspectionCaller, type InspectionRegistry, type InspectionSession,
  type InspectionSnapshot, type InspectionSpawn,
} from "./inspection-types.ts";

const TIMEOUT = 5_000;
const fail = (message: string): never => { throw new Error(`Paj inspection: ${message}`); };
const object = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value);
const string = (value: unknown): value is string =>
  typeof value === "string" && value.length > 0 && Buffer.byteLength(value) <= MAX_INSPECTION_TEXT_BYTES;
const keys = (value: Record<string, unknown>, allowed: string[]) => {
  if (Object.keys(value).some((key) => !allowed.includes(key))) fail("unexpected response fields");
};
function identity(value: unknown): asserts value is InspectionCaller {
  if (!object(value) || !string(value.id) || !string(value.piSessionId) ||
      !Number.isSafeInteger(value.pid) || (value.pid as number) <= 0) fail("invalid identity");
}
function same(a: InspectionCaller | InspectionSession, b: InspectionCaller | InspectionSession): boolean {
  return a.id === b.id && a.pid === b.pid && a.piSessionId === b.piSessionId;
}
function stable(a: InspectionSession, b: InspectionSession): boolean {
  return same(a, b) && a.role === b.role && a.parentPiSessionId === b.parentPiSessionId &&
    a.bridgeSocket === b.bridgeSocket && a.startedAtMs === b.startedAtMs;
}
function limitValue(limit = DEFAULT_INSPECTION_HISTORY): number {
  if (!Number.isInteger(limit) || limit < 1 || limit > MAX_INSPECTION_HISTORY) fail("invalid history limit");
  return limit;
}
export function inspectionSocketPath(session: InspectionSession): string {
  const path = session.bridgeSocket;
  if (!string(path) || !isAbsolute(path) || resolve(path) !== path || path.includes("\0") ||
      dirname(path) === "/" || path === join(dirname(path), "inspect.sock")) {
    fail("target has no valid inspection endpoint; reload target Paj extension");
  }
  const socket = join(dirname(path), "inspect.sock");
  if (Buffer.byteLength(socket) > 107) fail("socket path too long; shorten PAJ_RUNTIME_DIR");
  return socket;
}
function validateSessions(sessions: InspectionSession[]): void {
  if (!Array.isArray(sessions) || sessions.length > 10_000) fail("invalid registry");
  const ids = new Set<string>();
  const piIds = new Set<string>();
  for (const session of sessions) {
    if (!object(session) || !string(session.id) || !Number.isSafeInteger(session.pid) || session.pid <= 0 ||
        !string(session.role) || (session.piSessionId != null && !string(session.piSessionId)) || ids.has(session.id)) {
      fail("invalid registry identity");
    }
    ids.add(session.id);
    if (session.piSessionId != null && ["primary", "subagent"].includes(session.role)) {
      if (piIds.has(session.piSessionId)) fail("ambiguous Pi identity");
      piIds.add(session.piSessionId);
    }
    if (session.parentPiSessionId != null && !string(session.parentPiSessionId)) fail("invalid parent identity");
  }
}
function primary(session: InspectionSession): boolean {
  return session.role === "primary" && session.parentPiSessionId == null;
}
function spawnFor(child: InspectionSession, sessions: InspectionSession[], spawns: InspectionSpawn[]): InspectionSpawn {
  const parents = sessions.filter((s) => s.piSessionId === child.parentPiSessionId);
  if (child.role !== "subagent" || parents.length !== 1) fail("invalid child relationship");
  const records = spawns.filter((s) => s.childPiSessionId === child.piSessionId &&
    s.childPajSessionId === child.id && s.parentPiSessionId === parents[0].piSessionId &&
    s.parentPid === parents[0].pid);
  if (records.length !== 1) fail("missing or ambiguous owned spawn");
  return records[0];
}
export async function authorizeInspection(
  caller: InspectionCaller, target: InspectionSession, registry: InspectionRegistry,
): Promise<{ caller: InspectionSession; target: InspectionSession }> {
  const authorized = await authorizeFresh(caller, target, registry);
  return { caller: authorized.caller, target: authorized.target };
}
async function authorizeFresh(
  caller: InspectionCaller, target: InspectionSession, registry: InspectionRegistry,
): Promise<{ caller: InspectionSession; target: InspectionSession; scope: string }> {
  identity(caller);
  identity(target);
  const rawSessions = await registry.listSessions();
  validateSessions(rawSessions);
  const sessions = rawSessions.map((s) => ({ ...s }));
  const source = sessions.find((s) => same(s, caller));
  const destination = sessions.find((s) => same(s, target));
  if (!source || !destination || !stable(destination, target) || source.id === destination.id ||
      source.piSessionId === destination.piSessionId) fail("stale identity or self inspection");
  if (primary(source) && primary(destination)) return { caller: source, target: destination, scope: "primary" };
  const spawns = await registry.listSpawns();
  if (!Array.isArray(spawns) || spawns.length > 10_000 || spawns.some((s) =>
    !object(s) || !string(s.spawnId) || !string(s.parentPiSessionId) ||
    !Number.isSafeInteger(s.parentPid) || s.parentPid <= 0 ||
    (s.parentTaskId !== undefined && !string(s.parentTaskId)) ||
    (s.taskId !== undefined && !string(s.taskId)) ||
    (s.childPiSessionId != null && !string(s.childPiSessionId)) ||
    (s.childPajSessionId != null && !string(s.childPajSessionId)) ||
    (s.taskState !== undefined && s.taskState !== "active" && s.taskState !== "completed"))) fail("invalid spawn registry");
  const activeSpawn = (child: InspectionSession, seen = new Set<string>()): InspectionSpawn => {
    if (seen.has(child.id) || seen.size >= 32) fail("cyclic or excessive ownership ancestry");
    seen.add(child.id);
    const record = spawnFor(child, sessions, spawns);
    if (record.taskState !== "active") fail("caller has no active task");
    const parent = sessions.find((s) => s.piSessionId === child.parentPiSessionId)!;
    if (primary(parent)) {
      if (record.parentTaskId !== undefined) fail("invalid primary ownership scope");
    } else {
      const parentRecord = activeSpawn(parent, seen);
      if (record.parentTaskId !== (parentRecord.taskId ?? parentRecord.spawnId)) fail("caller ownership task is stale");
    }
    return record;
  };
  let current: InspectionSpawn | undefined;
  if (!primary(source)) current = activeSpawn(source);
  const owned = spawnFor(destination, sessions, spawns);
  if (destination.parentPiSessionId !== source.piSessionId || owned.parentPid !== source.pid ||
      owned.parentPiSessionId !== source.piSessionId ||
      (primary(source) ? owned.parentTaskId !== undefined :
        owned.parentTaskId !== (current!.taskId ?? current!.spawnId))) fail("inspection is not in current direct ownership scope");
  const scope = JSON.stringify([current, owned].map((record) => record ? [
    record.spawnId, record.parentPiSessionId, record.parentPid, record.parentTaskId,
    record.childPiSessionId, record.childPajSessionId, record.taskId, record.taskState,
  ] : null));
  return { caller: source, target: destination, scope };
}
function iso(value: unknown): boolean {
  return typeof value === "string" && value.length <= 32 && Number.isFinite(Date.parse(value)) &&
    new Date(value).toISOString() === value;
}
function validateSnapshot(value: unknown, target: InspectionSession, limit: number): InspectionSnapshot {
  if (!object(value)) fail("invalid snapshot");
  keys(value, ["version", "source", "identity", "observedAt", "status", "lastActivityAt", "history", "liveAssistant", "activeTools", "truncated", "limitations"]);
  if (value.version !== 1 || value.source !== "live-pi" || !object(value.identity)) fail("unsupported snapshot");
  const snapshotIdentity = value.identity;
  identity(snapshotIdentity);
  keys(snapshotIdentity, ["id", "pid", "piSessionId", "role", "name", "projectRoot"]);
  if (!same(snapshotIdentity, target) || snapshotIdentity.role !== target.role ||
      ["name", "projectRoot"].some((key) => snapshotIdentity[key] !== undefined && !string(snapshotIdentity[key])) ||
      !iso(value.observedAt) || Math.abs(Date.now() - Date.parse(value.observedAt as string)) > 30_000 ||
      (value.lastActivityAt !== null && !iso(value.lastActivityAt)) ||
      !["idle", "busy"].includes(value.status as string) || typeof value.truncated !== "boolean") fail("invalid snapshot identity or status");
  if (!Array.isArray(value.history) || value.history.length > limit) fail("invalid history");
  for (const entry of value.history) {
    if (!object(entry)) fail("invalid history entry");
    keys(entry, ["id", "role", "text", "truncated", "timestamp"]);
    if (!string(entry.id) || !["user", "assistant", "tool"].includes(entry.role as string) ||
        typeof entry.text !== "string" || Buffer.byteLength(entry.text) > MAX_INSPECTION_TEXT_BYTES ||
        (entry.truncated !== undefined && typeof entry.truncated !== "boolean") ||
        (entry.timestamp !== undefined && !iso(entry.timestamp))) fail("invalid history entry");
  }
  if (value.liveAssistant !== null) {
    if (!object(value.liveAssistant)) fail("invalid live assistant");
    keys(value.liveAssistant, ["text", "streaming", "truncated"]);
    if (typeof value.liveAssistant.text !== "string" || Buffer.byteLength(value.liveAssistant.text) > MAX_INSPECTION_TEXT_BYTES ||
        typeof value.liveAssistant.streaming !== "boolean" ||
        (value.liveAssistant.truncated !== undefined && typeof value.liveAssistant.truncated !== "boolean")) fail("invalid live assistant");
  }
  if (!Array.isArray(value.activeTools) || value.activeTools.length > 32) fail("invalid active tools");
  for (const tool of value.activeTools) {
    if (!object(tool)) fail("invalid tool");
    keys(tool, ["toolCallId", "toolName", "startedAt"]);
    if (!string(tool.toolCallId) || !string(tool.toolName) || !iso(tool.startedAt)) fail("invalid tool");
  }
  if (!Array.isArray(value.limitations) || value.limitations.length > 32 ||
      value.limitations.some((v) => !string(v)) || Buffer.byteLength(JSON.stringify(value)) > MAX_INSPECTION_SNAPSHOT_BYTES) fail("invalid snapshot bounds");
  return value as unknown as InspectionSnapshot;
}
async function privateDirectory(path: string, tighten = false): Promise<void> {
  const directory = dirname(path);
  const stat = await lstat(directory);
  if (!stat.isDirectory() || stat.isSymbolicLink() || stat.uid !== process.getuid?.() ||
      await realpath(directory) !== directory) fail("inspection directory must be private, owned and not symlinked");
  if (tighten && (stat.mode & 0o777) !== 0o700) await chmod(directory, 0o700);
  const current = await lstat(directory);
  if (current.dev !== stat.dev || current.ino !== stat.ino || (current.mode & 0o777) !== 0o700) {
    fail("inspection directory must be private, owned and not symlinked");
  }
}
async function privateSocket(path: string): Promise<void> {
  await privateDirectory(path);
  const stat = await lstat(path);
  if (!stat.isSocket() || stat.uid !== process.getuid?.() || (stat.mode & 0o777) !== 0o600) fail("unsafe inspection socket");
}
function readFrame(socket: Socket, requireEnd = false): Promise<unknown> {
  return new Promise((resolve, reject) => {
    let data = Buffer.alloc(0);
    let parsed = false;
    let result: unknown;
    const finish = (error?: Error, value?: unknown) => {
      socket.off("data", onData); socket.off("error", onError); socket.off("end", onEnd); socket.off("close", onEnd);
      if (!error && !requireEnd) socket.once("data", () => socket.destroy());
      error ? reject(error) : resolve(value);
    };
    const onError = (error: Error) => finish(error);
    const onEnd = () => parsed ? finish(undefined, result) : finish(new Error("Paj inspection: incomplete frame"));
    const onData = (chunk: Buffer) => {
      data = Buffer.concat([data, chunk]);
      if (data.length > MAX_INSPECTION_BYTES) return finish(new Error("Paj inspection: oversized frame"));
      const newline = data.indexOf(10);
      if (newline < 0) return;
      if (newline !== data.length - 1) return finish(new Error("Paj inspection: multiple frames"));
      try {
        result = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(data.subarray(0, newline)));
        parsed = true;
        if (!requireEnd) finish(undefined, result);
      } catch { finish(new Error("Paj inspection: malformed JSON")); }
    };
    socket.on("data", onData); socket.once("error", onError); socket.once("end", onEnd); socket.once("close", onEnd);
  });
}
function frame(value: unknown): string {
  const data = JSON.stringify(value) + "\n";
  if (Buffer.byteLength(data) > MAX_INSPECTION_BYTES) fail("oversized frame");
  return data;
}
export class InspectionServer {
  private server?: Server;
  private sockets = new Set<Socket>();
  private generation = 0;
  private operations: Promise<void> = Promise.resolve();
  private cleanup?: () => Promise<void>;
  private readonly options: {
    registry: InspectionRegistry;
    read: (limit: number) => InspectionSnapshot | Promise<InspectionSnapshot>;
  };
  constructor(options: {
    registry: InspectionRegistry;
    read: (limit: number) => InspectionSnapshot | Promise<InspectionSnapshot>;
  }) { this.options = options; }
  start(identity: InspectionSession): Promise<void> {
    const copy = { ...identity };
    const generation = this.generation;
    const operation = this.operations.then(async () => {
      if (generation !== this.generation) return;
      if (this.server) fail("server already running");
      const sessions = await this.options.registry.listSessions();
      validateSessions(sessions);
      if (!sessions.some((s) => stable(s, copy))) fail("stale server identity");
      const path = inspectionSocketPath(copy);
      await privateDirectory(path, true);
      try { await lstat(path); fail("inspection socket already exists; refusing to unlink a listener"); }
      catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
      if (generation !== this.generation) return;
      const server = createServer((socket) => {
        this.sockets.add(socket);
        socket.on("error", () => undefined);
        const deadline = setTimeout(() => socket.destroy(), TIMEOUT);
        socket.once("close", () => { clearTimeout(deadline); this.sockets.delete(socket); });
        void this.handle(socket, copy, generation).catch(() => socket.destroy());
      });
      server.maxConnections = 32;
      // Keep the temporary name shorter than inspect.sock for Unix socket path limits.
      const bindPath = join(dirname(path), `i${randomUUID().replaceAll("-", "").slice(0, 10)}`);
      await new Promise<void>((resolve, reject) => {
        server.once("error", reject);
        server.listen(bindPath, () => { server.off("error", reject); resolve(); });
      });
      server.on("error", () => undefined);
      this.server = server;
      let stat: Awaited<ReturnType<typeof lstat>>;
      try {
        await chmod(bindPath, 0o600);
        stat = await lstat(bindPath);
        await link(bindPath, path);
      } catch (error) {
        await this.close();
        throw error;
      }
      this.cleanup = async () => {
        const current = await lstat(path).catch(() => undefined);
        if (current?.dev === stat.dev && current?.ino === stat.ino && current.isSocket()) await unlink(path);
      };
      try { await chmod(path, 0o600); }
      catch (error) { await this.close(); throw error; }
    });
    this.operations = operation.catch(() => undefined);
    return operation;
  }
  stop(): Promise<void> {
    this.generation++;
    for (const socket of this.sockets) socket.destroy();
    const operation = this.operations.then(() => this.close());
    this.operations = operation.catch(() => undefined);
    return operation;
  }
  private async close(): Promise<void> {
    const server = this.server;
    this.server = undefined;
    for (const socket of this.sockets) socket.destroy();
    const cleanup = this.cleanup;
    this.cleanup = undefined;
    // Node unlinks its unique bind pathname; the public hard link is inode-checked separately.
    try { await cleanup?.(); }
    finally {
      if (server) await new Promise<void>((resolve) => server.close(() => resolve()));
    }
  }
  private async handle(socket: Socket, target: InspectionSession, generation: number): Promise<void> {
    const request = await readFrame(socket);
    if (!object(request)) fail("invalid request");
    keys(request, ["version", "method", "requestId", "caller", "target", "limit"]);
    if (request.version !== 1 || request.method !== "inspect" || !string(request.requestId)) fail("unsupported request");
    identity(request.caller); identity(request.target);
    keys(request.caller, ["id", "pid", "piSessionId"]);
    keys(request.target, ["id", "pid", "piSessionId"]);
    if (!same(request.target, target)) fail("target identity changed");
    const limit = limitValue(request.limit as number);
    const first = await authorizeFresh(request.caller, target, this.options.registry);
    const second = await authorizeFresh(request.caller, target, this.options.registry);
    if (!stable(first.caller, second.caller) || first.scope !== second.scope || generation !== this.generation || socket.destroyed) fail("inspection stopped or rebound");
    const snapshot = validateSnapshot(await this.options.read(limit), target, limit);
    const final = await authorizeFresh(request.caller, target, this.options.registry);
    if (!stable(second.caller, final.caller) || second.scope !== final.scope || generation !== this.generation || socket.destroyed) fail("inspection stopped or rebound");
    socket.end(frame({ version: 1, requestId: request.requestId, caller: request.caller, target: request.target, snapshot }));
  }
}
export async function inspectAgent(
  input: { caller: InspectionCaller; recipient: string; limit?: number },
  registry: InspectionRegistry,
  options: { signal?: AbortSignal; timeoutMs?: number } = {},
): Promise<InspectionSnapshot> {
  const limit = limitValue(input.limit);
  identity(input.caller);
  if (!string(input.recipient)) fail("invalid recipient");
  const timeout = options.timeoutMs ?? TIMEOUT;
  if (!Number.isFinite(timeout) || timeout <= 0 || timeout > 30_000) fail("invalid timeout");
  const controller = new AbortController();
  const abort = () => controller.abort(options.signal?.reason);
  options.signal?.addEventListener("abort", abort, { once: true });
  if (options.signal?.aborted) abort();
  const timer = setTimeout(() => controller.abort(new Error("Paj inspection: timeout")), timeout);
  let socket: Socket | undefined;
  try {
    const operation = async () => {
      const sessions = await registry.listSessions();
      validateSessions(sessions);
      const matches = sessions.filter((s) => s.id === input.recipient || s.piSessionId === input.recipient || s.name === input.recipient);
      if (matches.length !== 1) fail("recipient is missing or ambiguous");
      const target = { ...matches[0] };
      const authorized = await authorizeFresh(input.caller, target, registry);
      const path = inspectionSocketPath(target);
      await privateSocket(path).catch((error) => fail(`endpoint unavailable or unsafe; reload target Paj extension (${error.message})`));
      if (controller.signal.aborted) throw controller.signal.reason;
      const requestId = randomUUID();
      const caller = { id: input.caller.id, pid: input.caller.pid, piSessionId: input.caller.piSessionId };
      const expected = { id: target.id, pid: target.pid, piSessionId: target.piSessionId };
      socket = createConnection(path);
      const response = readFrame(socket, true);
      socket.on("error", () => undefined);
      socket.write(frame({ version: 1, method: "inspect", requestId, caller, target: expected, limit }));
      const value = await response;
      if (!object(value)) fail("invalid response");
      keys(value, ["version", "requestId", "caller", "target", "snapshot"]);
      identity(value.caller); identity(value.target);
      keys(value.caller, ["id", "pid", "piSessionId"]); keys(value.target, ["id", "pid", "piSessionId"]);
      if (value.version !== 1 || value.requestId !== requestId || !same(value.caller, caller) || !same(value.target, target)) fail("response identity mismatch");
      const snapshot = validateSnapshot(value.snapshot, target, limit);
      const final = await authorizeFresh(caller, target, registry);
      if (!stable(authorized.caller, final.caller) || authorized.scope !== final.scope) fail("caller identity changed");
      return snapshot;
    };
    return await Promise.race([
      operation(),
      new Promise<never>((_, reject) => {
        const onAbort = () => { socket?.destroy(); reject(controller.signal.reason ?? new Error("Paj inspection aborted")); };
        if (controller.signal.aborted) onAbort();
        else controller.signal.addEventListener("abort", onAbort, { once: true });
      }),
    ]);
  } catch (error) {
    throw new Error(`Paj inspection failed; reload target Paj extension if unavailable: ${error instanceof Error ? error.message : String(error)}`);
  } finally {
    clearTimeout(timer);
    options.signal?.removeEventListener("abort", abort);
    socket?.destroy();
  }
}
