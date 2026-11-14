import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, chmod, lstat, rm, symlink, unlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createServer, createConnection, type Socket } from "node:net";
import { InspectionServer, authorizeInspection, inspectAgent, inspectionSocketPath } from "../inspection.ts";
import { createInspectionView } from "../inspection-view.ts";
import { MAX_INSPECTION_SNAPSHOT_BYTES } from "../inspection-types.ts";
import type { InspectionSession, InspectionSnapshot, InspectionSpawn } from "../inspection-types.ts";

function session(id: string, parent?: string): InspectionSession {
  return { id, pid: 100 + id.charCodeAt(0), piSessionId: `pi-${id}`, role: parent ? "subagent" : "primary", parentPiSessionId: parent };
}
const caller = (s: InspectionSession) => ({ id: s.id, pid: s.pid, piSessionId: s.piSessionId! });
function snapshot(s: InspectionSession): InspectionSnapshot {
  return {
    version: 1, source: "live-pi", identity: { ...caller(s), role: s.role },
    observedAt: new Date().toISOString(), status: "idle", lastActivityAt: null,
    history: [], liveAssistant: null, activeTools: [], truncated: false, limitations: [],
  };
}
function registry(sessions: InspectionSession[], spawns: InspectionSpawn[] = []) {
  return { listSessions: async () => structuredClone(sessions), listSpawns: async () => structuredClone(spawns) };
}
function spawn(parent: InspectionSession, child: InspectionSession, extra = {}): InspectionSpawn {
  return { spawnId: `spawn-${child.id}`, parentPiSessionId: parent.piSessionId!, parentPid: parent.pid,
    childPiSessionId: child.piSessionId, childPajSessionId: child.id, taskState: "active", taskId: `task-${child.id}`, ...extra };
}
async function fixture(t: test.TestContext) {
  const dir = await mkdtemp(join(tmpdir(), "paj-i-"));
  await chmod(dir, 0o700);
  t.after(() => rm(dir, { recursive: true, force: true }));
  const a = session("a"), b = { ...session("b"), bridgeSocket: join(dir, "bridge.sock") };
  const sessions = [a, b];
  const reg = registry(sessions);
  return { dir, a, b, sessions, reg, path: inspectionSocketPath(b) };
}

test("fresh access permits primary-primary and direct scoped ownership only", async () => {
  const a = session("a"), b = session("b"), c = session("c", a.piSessionId), d = session("d", c.piSessionId);
  const spawns = [spawn(a, c), spawn(c, d, { parentTaskId: "task-c" })];
  const reg = registry([a, b, c, d], spawns);
  await authorizeInspection(caller(a), b, reg);
  await authorizeInspection(caller(a), c, reg);
  await authorizeInspection(caller(c), d, reg);
  for (const [source, target] of [[a, a], [a, d], [c, a], [c, b], [b, c], [d, c]]) {
    await assert.rejects(authorizeInspection(caller(source), target, reg));
  }
  spawns[1].parentTaskId = "old-task";
  await assert.rejects(authorizeInspection(caller(c), d, reg));
  spawns[1].parentTaskId = "task-c";
  spawns[0].taskState = "completed";
  await assert.rejects(authorizeInspection(caller(c), d, reg));
  spawns[0].taskState = "active";
  spawns[0].parentTaskId = "invalid-main-scope";
  await assert.rejects(authorizeInspection(caller(a), c, reg));
});

test("main inspection does not depend on unrelated subagent registry availability", async () => {
  const a = session("a"), b = session("b");
  const reg = { listSessions: async () => [a, b],
    listSpawns: async () => { throw new Error("Unrelated subagent metadata unavailable"); } };
  assert.equal((await authorizeInspection(caller(a), b, reg)).target.id, b.id);
});

test("unrelated legacy or custom-role registrations do not disable main inspection", async () => {
  const a = session("a"), b = session("b");
  const legacy = { ...session("legacy"), piSessionId: undefined };
  const custom = { ...session("custom"), role: "external" };
  const reg = registry([a, b, legacy, custom]);
  assert.equal((await authorizeInspection(caller(a), b, reg)).target.id, b.id);
  await assert.rejects(authorizeInspection(caller(a), legacy, reg));
  await assert.rejects(authorizeInspection(caller(a), custom, reg));
});

test("authorization rejects stale caller/target and malformed registry", async () => {
  const a = session("a"), b = session("b");
  const reg = registry([a, b]);
  await assert.rejects(authorizeInspection({ ...caller(a), pid: 1 }, b, reg));
  await assert.rejects(authorizeInspection(caller(a), { ...b, bridgeSocket: "/changed/socket" }, reg));
  await assert.rejects(authorizeInspection(caller(a), b, registry([a, b, b])));
  await assert.rejects(authorizeInspection(caller(a), b, registry([{ ...a, role: "external" }, b])));
});

test("read-only live round trip with exact names, identity and private socket", async (t) => {
  const { a, b, reg, path } = await fixture(t);
  b.name = "target";
  let reads = 0;
  const server = new InspectionServer({ registry: reg, read: (limit) => { reads++; assert.equal(limit, 20); return snapshot(b); } });
  t.after(() => server.stop());
  await server.start(b);
  assert.equal((await lstat(path)).mode & 0o777, 0o600);
  const result = await inspectAgent({ caller: caller(a), recipient: "target" }, reg);
  assert.equal(result.identity.id, b.id);
  assert.equal(reads, 1);
  await assert.rejects(inspectAgent({ caller: caller(a), recipient: "tar" }, reg));
  await assert.rejects(inspectAgent({ caller: caller(a), recipient: b.id, limit: 51 }, reg));
  await server.stop(); await server.stop();
  await assert.rejects(lstat(path));
  await server.start(b);
  await inspectAgent({ caller: caller(a), recipient: b.piSessionId! }, reg);
});

test("large live projections leave room for the transport envelope", async (t) => {
  const { a, b, reg } = await fixture(t);
  const view = createInspectionView();
  const branch = Array.from({ length: 50 }, (_, index) => ({ type: "message", id: `entry-${index}`,
    message: { role: "user", content: "x".repeat(1600) } }));
  const ctx = { isIdle: () => true,
    sessionManager: { getSessionId: () => b.piSessionId!, getBranch: () => branch } };
  const server = new InspectionServer({ registry: reg, read: limit => view.snapshot(b, ctx, limit) });
  t.after(() => server.stop());
  await server.start(b);
  const value = await inspectAgent({ caller: caller(a), recipient: b.id, limit: 50 }, reg);
  assert.equal(value.truncated, true);
  assert.ok(Buffer.byteLength(JSON.stringify(value)) < MAX_INSPECTION_SNAPSHOT_BYTES);
  assert.equal(value.history.at(-1)?.id, "entry-49");
});

test("socket publication fits the Linux Unix path limit including its temporary bind path", async (t) => {
  const { dir, a, b, reg } = await fixture(t);
  const parent = join(dir, "x".repeat(107 - Buffer.byteLength(dir) - 14));
  await mkdir(parent);
  b.bridgeSocket = join(parent, "bridge.sock");
  assert.equal(Buffer.byteLength(inspectionSocketPath(b)), 107);
  const server = new InspectionServer({ registry: reg, read: () => snapshot(b) });
  t.after(() => server.stop());
  await server.start(b);
  assert.equal((await inspectAgent({ caller: caller(a), recipient: b.id }, reg)).identity.id, b.id);
  assert.throws(() => inspectionSocketPath({ ...b, bridgeSocket: join(parent, "x", "bridge.sock") }), /too long/);
});

test("client refuses old targets, insecure directories and symlinks", async (t) => {
  const { dir, a, b, reg, path } = await fixture(t);
  await assert.rejects(inspectAgent({ caller: caller(a), recipient: b.id }, reg), /reload target/);
  await chmod(dir, 0o755);
  const server = new InspectionServer({ registry: reg, read: () => snapshot(b) });
  t.after(() => server.stop());
  await server.start(b);
  assert.equal((await lstat(dir)).mode & 0o777, 0o700);
  await chmod(dir, 0o755);
  await assert.rejects(inspectAgent({ caller: caller(a), recipient: b.id }, reg), /unsafe/);
  await server.stop();
  await chmod(dir, 0o700);
  await symlink("missing", path);
  await assert.rejects(server.start(b), /exists/);
  assert.equal((await lstat(path)).isSymbolicLink(), true);
  await assert.rejects(inspectAgent({ caller: caller(a), recipient: b.id }, reg), /unsafe/);
  for (const bridgeSocket of ["relative.sock", "/bridge.sock", `${dir}/../bridge.sock`, join(dir, "inspect.sock")]) {
    assert.throws(() => inspectionSocketPath({ ...b, bridgeSocket }));
  }
});

test("startup never unlinks a live listener, stale socket or regular file", async (t) => {
  const { b, reg, path } = await fixture(t);
  const first = new InspectionServer({ registry: reg, read: () => snapshot(b) });
  const second = new InspectionServer({ registry: reg, read: () => snapshot(b) });
  t.after(() => first.stop()); t.after(() => second.stop());
  await first.start(b);
  const ino = (await lstat(path)).ino;
  await assert.rejects(second.start(b), /exists/);
  assert.equal((await lstat(path)).ino, ino);
  await first.stop();
  await writeFile(path, "keep");
  await assert.rejects(second.start(b), /exists/);
  assert.equal((await lstat(path)).isFile(), true);
});

test("stop cleanup preserves a replacement public socket path", async (t) => {
  const { b, reg, path } = await fixture(t);
  const server = new InspectionServer({ registry: reg, read: () => snapshot(b) });
  await server.start(b);
  await unlink(path);
  await writeFile(path, "replacement");
  await server.stop();
  assert.equal((await lstat(path)).isFile(), true);
});

async function fake(t: test.TestContext, respond: (request: any, socket: Socket) => void) {
  const f = await fixture(t);
  const sockets = new Set<Socket>();
  const server = createServer((socket) => {
    sockets.add(socket); socket.on("error", () => undefined);
    socket.once("close", () => sockets.delete(socket));
    let data = "";
    socket.on("data", (chunk) => { data += chunk; if (data.includes("\n")) respond(JSON.parse(data), socket); });
  });
  await new Promise<void>((resolve) => server.listen(f.path, resolve));
  await chmod(f.path, 0o600);
  t.after(async () => {
    for (const socket of sockets) socket.destroy();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  });
  return f;
}
function response(request: any, s: InspectionSession) {
  return { version: 1, requestId: request.requestId, caller: request.caller, target: request.target, snapshot: snapshot(s) };
}

test("client rejects tampered frames, identity, private fields and excessive bounds", async (t) => {
  const mutations: ((value: any) => void)[] = [
    (v) => { v.requestId = "other"; },
    (v) => { v.target.pid++; },
    (v) => { v.caller.id = "other"; },
    (v) => { v.version = 2; },
    (v) => { v.snapshot.source = "disk"; },
    (v) => { v.snapshot.identity.id = "other"; },
    (v) => { v.snapshot.thinking = "secret"; },
    (v) => { v.snapshot.activeTools = [{ toolCallId: "a", toolName: "bash", startedAt: new Date().toISOString(), args: "secret" }]; },
    (v) => { v.snapshot.history = [{ id: "h", role: "assistant", text: "x".repeat(4097) }]; },
    (v) => { v.snapshot.history = [{ id: "h", role: "thinking", text: "secret" }]; },
    (v) => { v.snapshot.observedAt = "yesterday"; },
    (v) => { v.snapshot.liveAssistant = { text: "x", streaming: true, raw: "secret" }; },
  ];
  for (const mutate of mutations) await t.test(mutate.toString(), async (t) => {
    const f = await fake(t, (request, socket) => { const value = response(request, f.b); mutate(value); socket.end(JSON.stringify(value) + "\n"); });
    await assert.rejects(inspectAgent({ caller: caller(f.a), recipient: f.b.id }, f.reg));
  });
});

test("client bounds malformed/oversized/multiframe/timeout/abort responses", async (t) => {
  for (const payload of ["bad\n", "{}\n{}\n", "x".repeat(65537)]) await t.test(payload.slice(0, 20), async (t) => {
    const f = await fake(t, (_, socket) => socket.end(payload));
    await assert.rejects(inspectAgent({ caller: caller(f.a), recipient: f.b.id }, f.reg));
  });
  await t.test("timeout and abort", async (t) => {
    const f = await fake(t, () => undefined);
    await assert.rejects(inspectAgent({ caller: caller(f.a), recipient: f.b.id }, f.reg, { timeoutMs: 20 }), /timeout/);
    const controller = new AbortController();
    const pending = inspectAgent({ caller: caller(f.a), recipient: f.b.id }, f.reg, { signal: controller.signal });
    controller.abort();
    await assert.rejects(pending, /abort/i);
  });
});

test("server validates caller, target and methods before invoking read", async (t) => {
  const { a, b, reg, path } = await fixture(t);
  let reads = 0;
  const server = new InspectionServer({ registry: reg, read: () => { reads++; return snapshot(b); } });
  t.after(() => server.stop());
  await server.start(b);
  for (const change of [(v: any) => { v.method = "send"; }, (v: any) => { v.caller.pid++; },
    (v: any) => { v.target.id = "other"; }, (v: any) => { v.limit = 100; }]) {
    await new Promise<void>((resolve) => {
      const socket = createConnection(path);
      socket.on("error", () => undefined); socket.once("close", () => resolve());
      const request = { version: 1, method: "inspect", requestId: "test", caller: caller(a), target: caller(b), limit: 20 };
      change(request); socket.write(JSON.stringify(request) + "\n");
    });
  }
  assert.equal(reads, 0);
});

test("stop invalidates pending reads across restart", async (t) => {
  const { a, b, reg } = await fixture(t);
  let finish!: (value: InspectionSnapshot) => void;
  let entered!: () => void;
  const entry = new Promise<void>((resolve) => { entered = resolve; });
  const server = new InspectionServer({ registry: reg, read: () => { entered(); return new Promise((resolve) => { finish = resolve; }); } });
  t.after(() => server.stop());
  await server.start(b);
  const pending = inspectAgent({ caller: caller(a), recipient: b.id }, reg);
  const rejected = assert.rejects(pending);
  await entry;
  await server.stop();
  await server.start(b);
  finish(snapshot(b));
  await rejected;
});

test("fresh client validation rejects registry rebound during response", async (t) => {
  const f = await fake(t, (request, socket) => {
    f.sessions[0] = { ...f.a, pid: f.a.pid + 1 };
    socket.end(JSON.stringify(response(request, f.b)) + "\n");
  });
  await assert.rejects(inspectAgent({ caller: caller(f.a), recipient: f.b.id }, f.reg), /stale/);
});

test("server rejects task-scope changes while a snapshot read is pending", async (t) => {
  const { dir, a, b } = await fixture(t);
  const c = { ...session("c", a.piSessionId), bridgeSocket: join(dir, "bridge.sock") };
  const records = [spawn(a, c)];
  const reg = registry([a, b, c], records);
  const server = new InspectionServer({ registry: reg, read: () => {
    records[0].taskId = "reassigned";
    return snapshot(c);
  } });
  t.after(() => server.stop());
  await server.start(c);
  await assert.rejects(inspectAgent({ caller: caller(a), recipient: c.id }, reg));
});

test("client rejects a second response frame arriving in a later chunk", async (t) => {
  const f = await fake(t, (request, socket) => {
    socket.write(JSON.stringify(response(request, f.b)) + "\n");
    setTimeout(() => socket.end("{}\n"), 10);
  });
  await assert.rejects(inspectAgent({ caller: caller(f.a), recipient: f.b.id }, f.reg), /frames/);
});

test("client timeout also bounds registry lookup without connecting", async () => {
  const reg = { listSessions: () => new Promise<InspectionSession[]>(() => undefined), listSpawns: async () => [] };
  await assert.rejects(inspectAgent({ caller: caller(session("a")), recipient: "b" }, reg, { timeoutMs: 10 }), /timeout/);
});

test("queued start followed immediately by stop never publishes an endpoint", async (t) => {
  const { b, reg, path } = await fixture(t);
  const server = new InspectionServer({ registry: reg, read: () => snapshot(b) });
  const starting = server.start(b);
  const stopping = server.stop();
  await Promise.all([starting, stopping]);
  await assert.rejects(lstat(path));
  await server.start(b);
  await server.stop();
});
