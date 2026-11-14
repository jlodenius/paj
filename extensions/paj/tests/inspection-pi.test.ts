import assert from "node:assert/strict";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import { createServer } from "node:http";
import test from "node:test";

const execute = promisify(execFile);
const extension = fileURLToPath(new URL("../index.ts", import.meta.url));

function sessionFile(root: string) {
  const file = join(root, "target.jsonl");
  const time = new Date().toISOString();
  const assistant = (text: string) => ({ role: "assistant", content: [
    { type: "thinking", thinking: "NEVER_THINKING" },
    { type: "text", text },
  ], api: "openai-completions", provider: "openai", model: "gpt-4o", stopReason: "stop",
  usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } }, timestamp: Date.now() });
  const records = [
    { type: "session", version: 3, id: randomUUID(), timestamp: time, cwd: root },
    { type: "message", id: "00000001", parentId: null, timestamp: time,
      message: { role: "user", content: "Review this fixture", timestamp: Date.now() } },
    { type: "message", id: "00000002", parentId: "00000001", timestamp: time,
      message: assistant("NEVER_ABANDONED_BRANCH") },
    { type: "message", id: "00000003", parentId: "00000001", timestamp: time,
      message: assistant("Work is visible") },
    { type: "message", id: "00000004", parentId: "00000003", timestamp: time,
      message: { role: "toolResult", toolCallId: "fixture-tool", toolName: "read",
        content: [{ type: "text", text: "NEVER_RAW_TOOL_RESULT" }], isError: false,
        timestamp: Date.now() } },
    { type: "message", id: "00000005", parentId: "00000004", timestamp: time,
      message: assistant("Visible fixture final report") },
    { type: "custom_message", id: "00000006", parentId: "00000005", timestamp: time,
      customType: "fixture-hidden", content: "NEVER_HIDDEN_CUSTOM", display: false },
  ];
  writeFileSync(file, records.map(record => JSON.stringify(record)).join("\n") + "\n");
  return file;
}

function connection(root: string, env: NodeJS.ProcessEnv, name: string, file?: string, allowRun = false) {
  const child = spawn(process.env.PAJ_INSPECTION_PI_BIN!, ["--mode", "rpc", "--no-extensions",
    "-e", join(root, "fixture.ts"), "--name", name, ...(file ? ["--session", file] : [])],
  { cwd: root, env, stdio: ["pipe", "pipe", "pipe"] });
  const pending = new Map<string, { resolve: (record: any) => void; reject: (error: Error) => void }>();
  const events: any[] = [];
  let buffer = "";
  let errors = "";
  let sequence = 0;
  child.stderr.on("data", chunk => { errors += chunk; });
  child.stdout.setEncoding("utf8");
  child.stdout.on("data", chunk => {
    buffer += chunk;
    let end;
    while ((end = buffer.indexOf("\n")) >= 0) {
      const line = buffer.slice(0, end);
      buffer = buffer.slice(end + 1);
      if (!line.startsWith("{")) continue;
      const record = JSON.parse(line);
      events.push(record);
      if (record.type === "agent_start" && !allowRun) child.kill("SIGTERM");
      const waiting = pending.get(record.id);
      if (record.type === "response" && waiting) {
        pending.delete(record.id);
        waiting.resolve(record);
      }
    }
  });
  child.on("exit", () => {
    for (const waiting of pending.values()) waiting.reject(new Error(`Pi exited: ${errors}`));
    pending.clear();
  });
  async function request(type: string, values: object = {}) {
    const id = `inspection-${++sequence}`;
    const response = new Promise<any>((resolve, reject) => pending.set(id, { resolve, reject }));
    child.stdin.write(`${JSON.stringify({ id, type, ...values })}\n`);
    const result = await response;
    assert.equal(result.success, true, result.error || errors);
    return result.data;
  }
  async function inspect(recipient: string) {
    const before = events.length;
    const response = await request("prompt", {
      message: `/inspection-test ${JSON.stringify({ recipient, limit: 20 })}`,
    });
    assert.equal(response.disposition, "handled");
    return events.slice(before).find(event => event.method === "notify");
  }
  async function close() {
    if (child.exitCode !== null || child.signalCode !== null) return;
    const exited = new Promise(resolve => child.once("exit", resolve));
    child.stdin.end();
    const timer = setTimeout(() => child.kill("SIGKILL"), 4000);
    await exited;
    clearTimeout(timer);
  }
  return { request, inspect, close, events, allowRun, diagnostics: () => errors };
}

test("real Pi inspection reads active-branch evidence without waking main or owned subagent targets",
  { skip: !process.env.PAJ_INSPECTION_PI_BIN, timeout: 45000 }, async t => {
  const root = mkdtempSync(join(tmpdir(), "iv-"));
  const connections: ReturnType<typeof connection>[] = [];
  let cleanupTmux: (() => Promise<unknown>) | undefined;
  let cleanupProvider: (() => Promise<unknown>) | undefined;
  let finishLive: (() => void) | undefined;
  t.after(async () => {
    finishLive?.();
    await cleanupTmux?.();
    for (const client of connections.reverse()) await client.close();
    await cleanupProvider?.();
    for (const client of connections) {
      if (!client.allowRun) assert.ok(!client.events.some(event => event.type === "agent_start"));
    }
    rmSync(root, { recursive: true, force: true });
  });
  for (const dir of ["a", "r", "tmux"]) mkdirSync(join(root, dir), { mode: 0o700 });
  writeFileSync(join(root, "a", "settings.json"), JSON.stringify({
    defaultProjectTrust: "always", cacheWarming: "off",
  }));
  writeFileSync(join(root, "fixture.ts"), `import extension from ${JSON.stringify(extension)};
export default function(pi) {
  let inspection;
  extension(new Proxy(pi, { get(target,key) {
    if(key==='registerTool') return tool=>{
      if(tool.name==='inspect_agent') inspection=tool;
      return target.registerTool(tool);
    };
    return Reflect.get(target,key);
  }}));
  pi.registerCommand('inspection-test', { description:'Isolated test inspection',
    handler:async(args,ctx)=>{
      try {
        const value=await inspection.execute('inspection-test-call',JSON.parse(args),undefined,undefined,ctx);
        ctx.ui.notify(JSON.stringify(value.details),'info');
      } catch(error) { ctx.ui.notify(String(error.message),'error'); }
    }
  });
}`);
  const env = Object.fromEntries(Object.entries(process.env)
    .filter(([key]) => !/API_KEY|TOKEN|SECRET|CREDENTIAL|^PAJ_|^CAPTAIN_|^PI_CAPTAIN_/.test(key)));
  Object.assign(env, { HOME: root, PI_CODING_AGENT_DIR: join(root, "a"),
    PAJ_RUNTIME_DIR: join(root, "r", "p"), XDG_RUNTIME_DIR: join(root, "r"),
    XDG_CONFIG_HOME: join(root, "c"), TMUX_TMPDIR: join(root, "tmux") });
  cleanupTmux = () => execute("tmux", ["-L", "paj", "kill-server"], { env, timeout: 2000 }).catch(() => undefined);
  const paj = async (args: string[]) => JSON.parse((await execute(
    process.env.PAJ_INSPECTION_PAJ_BIN || "paj", ["--json", ...args],
    { cwd: root, env, timeout: 5000 })).stdout);
  const caller = connection(root, env, "inspection-caller");
  connections.push(caller);
  const commands = await caller.request("get_commands");
  assert.ok(commands.commands.some((value: any) => value.name === "inspection-test"));
  const target = connection(root, env, "inspection-target", sessionFile(root));
  connections.push(target);
  await target.request("get_commands");
  const sessions = await paj(["session", "list", "--all"]);
  const main = sessions.find((value: any) => value.name === "inspection-target");
  const owner = sessions.find((value: any) => value.name === "inspection-caller");
  assert.ok(main && owner);
  const before = await target.request("get_entries");
  const observed = await caller.inspect(main.id);
  assert.equal(observed?.notifyType, "info", `${JSON.stringify(observed)}\n${target.diagnostics()}`);
  const snapshot = JSON.parse(observed.message);
  assert.equal(snapshot.identity.id, main.id);
  assert.equal(snapshot.source, "live-pi");
  assert.equal(snapshot.status, "idle");
  assert.ok(snapshot.history.some((row: any) => row.text.includes("Visible fixture final report")));
  assert.ok(!JSON.stringify(snapshot).includes("NEVER_"));
  assert.deepEqual(await target.request("get_entries"), before);
  assert.equal((await target.request("get_state")).isStreaming, false);
  assert.ok(!target.events.some(event => event.type === "agent_start"));
  assert.equal((await target.request("prompt", { message: "/agent-rename inspection-renamed" })).disposition, "handled");
  const renamed = await caller.inspect("inspection-renamed");
  assert.equal(renamed?.notifyType, "info", JSON.stringify(renamed));
  assert.equal(JSON.parse(renamed.message).identity.name, "inspection-renamed");

  writeFileSync(join(root, "task.txt"), "Read-only isolated ownership fixture");
  const record = await paj(["subagent", "create", "--parent-pi-session-id", owner.piSessionId,
    "--parent-pid", String(owner.pid), "--cwd", root, "--project-root", root,
    "--task-file", join(root, "task.txt")]);
  await execute("tmux", ["-L", "paj", "-f", "/dev/null", "new-session", "-d", "-s", record.tmuxName,
    "sleep", "60"], { env, timeout: 5000 });
  const child = connection(root, { ...env, PAJ_ROLE: "subagent", PAJ_SPAWN_ID: record.spawnId,
    PAJ_PARENT_PI_SESSION_ID: owner.piSessionId }, "inspection-child");
  connections.push(child);
  await child.request("get_commands");
  const childSession = (await paj(["session", "list", "--all"]))
    .find((value: any) => value.name === "inspection-child");
  assert.ok(childSession);
  const ownChild = await caller.inspect(childSession.id);
  assert.equal(ownChild?.notifyType, "info", `${JSON.stringify(ownChild)}\n${child.diagnostics()}\n`
    + JSON.stringify(await paj(["subagent", "list", "--all"])));
  assert.equal(JSON.parse(ownChild.message).identity.role, "subagent");
  const unrelated = await target.inspect(childSession.id);
  assert.equal(unrelated?.notifyType, "error");
  const parentAccess = await child.inspect(owner.id);
  assert.equal(parentAccess?.notifyType, "error");
  assert.ok(!child.events.some(event => event.type === "agent_start"));
  await child.close();
  await cleanupTmux();
  await paj(["subagent", "remove", record.spawnId]);

  let providerRequests = 0;
  const provider = createServer((request, response) => {
    request.resume();
    providerRequests++;
    response.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
    const chunk = (delta: object, finish: string | null = null) => `data: ${JSON.stringify({
      id: "fixture", object: "chat.completion.chunk", created: Math.floor(Date.now() / 1000),
      model: "fixture", choices: [{ index: 0, delta, finish_reason: finish }],
    })}\n\n`;
    response.write(chunk({ role: "assistant", content: "LIVE_FIXTURE_PROGRESS" }));
    finishLive = () => { if (!response.writableEnded) response.end(chunk({}, "stop") + "data: [DONE]\n\n"); };
  });
  await new Promise<void>(resolve => provider.listen(0, "127.0.0.1", resolve));
  cleanupProvider = () => new Promise<void>(resolve => { provider.closeAllConnections(); provider.close(() => resolve()); });
  const address = provider.address();
  assert.ok(address && typeof address === "object");
  const liveAgent = join(root, "live-agent");
  mkdirSync(liveAgent, { mode: 0o700 });
  writeFileSync(join(liveAgent, "settings.json"), JSON.stringify({ defaultProjectTrust: "always",
    cacheWarming: "off", defaultProvider: "inspection-fixture", defaultModel: "fixture" }));
  writeFileSync(join(liveAgent, "models.json"), JSON.stringify({ providers: {
    "inspection-fixture": { baseUrl: `http://127.0.0.1:${address.port}/v1`, api: "openai-completions",
      apiKey: "local-test-only", models: [{ id: "fixture", reasoning: false, input: ["text"],
        contextWindow: 128000, maxTokens: 256,
        cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }] },
  } }));
  const live = connection(root, { ...env, PI_CODING_AGENT_DIR: liveAgent }, "inspection-live", undefined, true);
  connections.push(live);
  await live.request("get_commands");
  const liveSession = (await paj(["session", "list", "--all"]))
    .find((value: any) => value.name === "inspection-live");
  assert.ok(liveSession);
  assert.equal((await live.request("prompt", { message: "Produce the local streaming fixture." })).disposition, "started");
  const deadline = Date.now() + 8000;
  while (!live.events.some(event => event.type === "message_update"
    && JSON.stringify(event).includes("LIVE_FIXTURE_PROGRESS"))) {
    assert.ok(Date.now() < deadline, live.diagnostics() + JSON.stringify(live.events));
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  const streamingBefore = await live.request("get_entries");
  const liveObservation = await caller.inspect(liveSession.id);
  assert.equal(liveObservation?.notifyType, "info", JSON.stringify(liveObservation));
  const streaming = JSON.parse(liveObservation.message);
  assert.equal(streaming.status, "busy");
  assert.ok(streaming.liveAssistant?.text.includes("LIVE_FIXTURE_PROGRESS"));
  assert.equal(providerRequests, 1);
  assert.equal(live.events.filter(event => event.type === "agent_start").length, 1);
  assert.deepEqual(await live.request("get_entries"), streamingBefore);
  finishLive!();
  while (!live.events.some(event => event.type === "agent_settled")) {
    assert.ok(Date.now() < deadline, live.diagnostics());
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  assert.equal(providerRequests, 1);
});
