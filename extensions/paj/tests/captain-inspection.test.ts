import test from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtemp, chmod, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createConnection } from "node:net";
import { InspectionServer, authorizeInspection, inspectAgent, inspectionSocketPath } from "../inspection.ts";
import type { InspectionSession, InspectionSpawn, InspectionSnapshot } from "../inspection-types.ts";

const token = () => {
  const id = randomUUID();
  return `${id.slice(0, 14)}7${id.slice(15)}`;
};
function fixture() {
  const captain: InspectionSession = { id: "captain", pid: 1, piSessionId: "pi-captain", role: "primary" };
  const parent: InspectionSession = { id: "parent", pid: 2, piSessionId: "pi-parent", role: "primary" };
  const child: InspectionSession = { id: "child", pid: 3, piSessionId: "pi-child", role: "subagent",
    parentPiSessionId: parent.piSessionId };
  const owned: InspectionSpawn = { spawnId: "spawn", parentPid: parent.pid,
    parentPiSessionId: parent.piSessionId!, childPiSessionId: child.piSessionId,
    childPajSessionId: child.id, taskId: "task", taskState: "active" };
  const sessions = [captain, parent, child];
  const spawns = [owned];
  const registry = { listSessions: async () => structuredClone(sessions),
    listSpawns: async () => structuredClone(spawns) };
  const caller = { id: captain.id, pid: captain.pid, piSessionId: captain.piSessionId! };
  return { captain, parent, child, owned, sessions, spawns, registry, caller };
}
function snapshot(child: InspectionSession): InspectionSnapshot {
  return { version: 1, source: "live-pi", identity: { id: child.id, pid: child.pid,
    piSessionId: child.piSessionId!, role: child.role }, observedAt: new Date().toISOString(),
    status: "idle", lastActivityAt: null, history: [{ id: "visible", role: "assistant", text: "Visible" }],
    liveAssistant: null, activeTools: [], truncated: false, limitations: [] };
}

test("only fresh registry Captain capability permits a different main's recorded child", async () => {
  const f = fixture();
  const inspect = () => authorizeInspection(f.caller, f.child, f.registry);
  await assert.rejects(inspect());
  const unverified = { ...f.caller, captain: true };
  await assert.rejects(authorizeInspection(unverified, f.child, f.registry));
  f.captain.captainInspection = "true";
  await assert.rejects(inspect());
  f.captain.captainInspection = token();
  await inspect();
  f.owned.taskState = "completed";
  await inspect();
  delete f.captain.captainInspection;
  await assert.rejects(inspect());
});

test("Captain requires a live unambiguous main parent and matching recorded ownership", async () => {
  for (const mutate of [
    (f: ReturnType<typeof fixture>) => { f.sessions.splice(1, 1); },
    (f: ReturnType<typeof fixture>) => { f.parent.pid++; },
    (f: ReturnType<typeof fixture>) => { f.owned.childPajSessionId = "stale"; },
    (f: ReturnType<typeof fixture>) => { f.owned.childPiSessionId = "stale"; },
    (f: ReturnType<typeof fixture>) => { f.owned.parentPiSessionId = "stale"; },
    (f: ReturnType<typeof fixture>) => { f.owned.parentTaskId = "stale"; },
    (f: ReturnType<typeof fixture>) => { f.parent.role = "subagent"; },
    (f: ReturnType<typeof fixture>) => { f.spawns.push({ ...f.owned }); },
    (f: ReturnType<typeof fixture>) => { f.captain.role = "subagent"; },
  ]) {
    const f = fixture();
    f.captain.captainInspection = token();
    mutate(f);
    await assert.rejects(authorizeInspection(f.caller, f.child, f.registry));
  }
});

async function live(t: test.TestContext) {
  const f = fixture();
  const dir = await mkdtemp(join(tmpdir(), "paj-ci-"));
  await chmod(dir, 0o700);
  f.child.bridgeSocket = join(dir, "bridge.sock");
  f.captain.captainInspection = token();
  t.after(() => rm(dir, { recursive: true, force: true }));
  return f;
}

test("receiver checks trusted state even when a client sends an unverified Captain flag", async t => {
  const f = await live(t);
  delete f.captain.captainInspection;
  let reads = 0;
  const server = new InspectionServer({ registry: f.registry, read: () => { reads++; return snapshot(f.child); } });
  t.after(() => server.stop());
  await server.start(f.child);
  for (const forged of [false, true]) {
    const result = await new Promise<string>((resolve, reject) => {
      const socket = createConnection(inspectionSocketPath(f.child));
      let output = "";
      socket.on("data", chunk => { output += chunk; });
      socket.on("error", reject);
      socket.on("close", () => resolve(output));
      socket.write(JSON.stringify({ version: 1, method: "inspect", requestId: "forged",
        caller: { ...f.caller, ...(forged ? { captain: true } : {}) },
        target: { id: f.child.id, pid: f.child.pid, piSessionId: f.child.piSessionId }, limit: 20 }) + "\n");
    });
    assert.equal(result, "");
  }
  assert.equal(reads, 0);
});

test("final ownership lookup cannot hide Captain revocation from receiver or client", async t => {
  for (const side of ["receiver", "client"] as const) {
    for (const change of ["off", "reenable", "identity"] as const) {
      await t.test(`${side} ${change}`, async t => {
        const f = await live(t);
        let lookups = 0;
        const racing = { ...f.registry, listSpawns: async () => {
          const records = await f.registry.listSpawns();
          if (++lookups === (side === "receiver" ? 3 : 2)) {
            if (change === "off") delete f.captain.captainInspection;
            if (change === "reenable") f.captain.captainInspection = token();
            if (change === "identity") f.captain.pid++;
          }
          return records;
        } };
        const server = new InspectionServer({ registry: side === "receiver" ? racing : f.registry,
          read: () => snapshot(f.child) });
        t.after(() => server.stop());
        await server.start(f.child);
        await assert.rejects(inspectAgent({ caller: f.caller, recipient: f.child.id },
          side === "client" ? racing : f.registry));
      });
    }
  }
});

test("Captain live socket inspection succeeds but fences revocation, re-enable and ownership change", async t => {
  for (const mutation of ["off", "reenable", "parent", "task", "caller"] as const) {
    await t.test(mutation, async t => {
      const f = await live(t);
      let release: (() => void) | undefined;
      let entered: (() => void) | undefined;
      const reading = new Promise<void>(resolve => { entered = resolve; });
      let pause = false;
      const server = new InspectionServer({ registry: f.registry, read: async () => {
        if (pause) {
          entered!();
          await new Promise<void>(resolve => { release = resolve; });
        }
        return snapshot(f.child);
      } });
      t.after(() => server.stop());
      await server.start(f.child);
      const request = () => inspectAgent({ caller: f.caller, recipient: f.child.id }, f.registry);
      assert.equal((await request()).history[0].text, "Visible");
      pause = true;
      const pending = assert.rejects(request());
      await reading;
      if (mutation === "off") delete f.captain.captainInspection;
      if (mutation === "reenable") f.captain.captainInspection = token();
      if (mutation === "parent") f.parent.pid++;
      if (mutation === "task") f.owned.taskId = "new-task";
      if (mutation === "caller") f.captain.pid++;
      release!();
      await pending;
    });
  }
});
