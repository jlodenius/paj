import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { registerHooks } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const hooks = registerHooks({
  resolve(specifier, context, next) {
    const source =
      specifier === "@earendil-works/pi-coding-agent"
        ? "export const getAgentDir = () => process.env.PI_CODING_AGENT_DIR;"
        : specifier === "typebox"
          ? "export const Type = new Proxy({}, { get: (_, type) => (...args) => ({ type, args }) });"
          : undefined;
    return source
      ? {
          url: `data:text/javascript,${encodeURIComponent(source)}`,
          shortCircuit: true,
        }
      : next(specifier, context);
  },
});
const { default: extension } = await import("../index.ts");
hooks.deregister();

type Item = Record<string, any>;

async function harness(options: { child?: boolean; busy?: boolean } = {}) {
  const directory = await mkdtemp(join(tmpdir(), "paj-integration-"));
  const env = { ...process.env };
  process.env.PI_CODING_AGENT_DIR = directory;
  if (options.child) process.env.PAJ_SPAWN_ID = "spawn";
  else delete process.env.PAJ_SPAWN_ID;
  await writeFile(
    join(directory, "paj.json"),
    JSON.stringify({
      subagents: {
        model: {
          rules: [{ parentFamily: "astra", childFamily: "sol" }],
          provider: "same",
          version: "latest",
          fallback: "inherit",
        },
      },
    }),
  );
  const models = [
    { provider: "openai-codex", id: "gpt-6-astra" },
    { provider: "openai-codex", id: "gpt-6-sol" },
    { provider: "openai-codex", id: "gpt-6.1-sol" },
    { provider: "claude-bridge", id: "claude-opus-5" },
  ];
  let idle = !options.busy;
  const commands = new Map<string, Item>();
  const tools = new Map<string, Item>();
  const events = new Map<string, (...args: any[]) => any>();
  const calls: Array<{ command: string; args: string[] }> = [];
  const messages: Item[] = [];
  const notices: string[] = [];
  const branch: Item[] = [];
  const intervals: Array<() => void> = [];
  let inbox: Item[] = [];
  let children: Item[] = [];
  let record: Item = {
    spawnId: "spawn",
    taskId: "task",
    taskState: "active",
    task: "Run tests",
    parentPiSessionId: "parent",
    childPajSessionId: "session",
    tmuxName: "paj-test",
  };
  let session: Item;
  const ctx = {
    cwd: directory,
    hasUI: false,
    mode: "rpc",
    model: models[0],
    isIdle: () => idle,
    abort() {},
    ui: { notify: (text: string) => notices.push(text) },
    sessionManager: {
      getSessionId: () => "pi-session",
      getBranch: () => branch,
    },
    modelRegistry: { getAvailable: () => models },
  };
  const pi = {
    on: (name: string, callback: (...args: any[]) => any) =>
      events.set(name, callback),
    registerTool: (tool: Item) => tools.set(tool.name, tool),
    registerCommand: (name: string, command: Item) =>
      commands.set(name, command),
    getSessionName: () => "test-agent",
    getActiveTools: () => [...tools.keys()],
    setActiveTools() {},
    appendEntry: (customType: string, data: unknown) =>
      branch.push({ type: "custom", customType, data }),
    sendMessage: (message: Item, delivery: Item) => {
      messages.push({ ...message, delivery });
      branch.push({ type: "custom_message", ...message });
      if (delivery.triggerTurn) idle = false;
    },
    sendUserMessage: () =>
      assert.fail("agent messages must not impersonate the user"),
    exec: async (command: string, args: string[]) => {
      calls.push({ command, args });
      let response: unknown = {};
      const value = (flag: string) => args[args.indexOf(flag) + 1];
      const parts = args.filter((arg) => arg !== "--json");
      if (command.endsWith("/tmux-agent")) {
        assert.equal(
          await readFile(value("--task-file"), "utf8"),
          "finite work",
        );
        response = { ...record, requestedModel: value("--model") };
      } else if (parts[0] === "session" && parts[1] === "register") {
        session = {
          id: "session",
          piSessionId: "pi-session",
          pid: process.pid,
          name: "test-agent",
          status: "idle",
          model: value("--model"),
          bridgeSocket: join(directory, "bridge.sock"),
        };
        response = session;
      } else if (parts[0] === "session" && parts[1] === "show") {
        response = session;
      } else if (parts[0] === "session" && parts[1] === "model") {
        session.model = parts[3];
      } else if (parts[0] === "session" && parts[1] === "list") {
        response = [session];
      } else if (parts[0] === "subagent") {
        if (parts[1] === "list") response = children;
        else if (parts[1] === "flush") {
          record = {
            ...record,
            result: record.result
              ? { ...record.result, delivered: true }
              : undefined,
          };
          response = record;
        } else if (parts[1] === "complete") {
          assert.equal(value("--task-id"), record.taskId);
          record = { ...record, taskState: "completed" };
          response = record;
        } else if (parts[1] === "assign") {
          record = {
            ...record,
            task: await readFile(value("--task-file"), "utf8"),
            taskId: "new-task",
            taskState: "active",
          };
          response = record;
        } else response = record;
      } else if (parts[0] === "message" && parts[1] === "pending") {
        response = [...inbox];
      } else if (parts[0] === "message" && parts[1] === "ack") {
        inbox = inbox.filter((message) => message.id !== parts[3]);
      } else if (parts[0] === "message" && parts[1] === "send") {
        response = { id: "outbound", kind: value("--kind") };
      }
      return { code: 0, stdout: JSON.stringify(response), stderr: "" };
    },
  };
  const flush = () => new Promise<void>((resolve) => setImmediate(resolve));
  const start = async () => {
    intervals.length = 0;
    const originalInterval = globalThis.setInterval;
    globalThis.setInterval = ((callback: () => void) => {
      intervals.push(callback);
      return originalInterval(() => {}, 2 ** 30);
    }) as typeof setInterval;
    try {
      extension(pi as never);
      await events.get("session_start")!({}, ctx);
    } finally {
      globalThis.setInterval = originalInterval;
    }
    await flush();
  };
  await start();
  return {
    commands,
    tools,
    calls,
    messages,
    notices,
    branch,
    ctx,
    setIdle: (value: boolean) => {
      idle = value;
    },
    setRecord: (value: Item) => {
      record = { ...record, ...value };
    },
    setInbox: (value: Item[]) => {
      inbox = value;
    },
    setChildren: (value: Item[]) => {
      children = value;
    },
    reload: async () => {
      await events.get("session_shutdown")!({ reason: "reload" }, ctx);
      await start();
    },
    inbox: () => inbox,
    event: async (name: string, value: Item = {}) =>
      events.get(name)!(value, ctx),
    command: async (name: string, args = "") =>
      commands.get(name)!.handler(args, ctx),
    tool: async (name: string, params: Item) =>
      tools.get(name)!.execute("call", params, undefined, undefined, ctx),
    poll: async () => {
      intervals[1]();
      await flush();
    },
    close: async () => {
      await events.get("session_shutdown")!({ reason: "reload" }, ctx);
      for (const key of Object.keys(process.env))
        if (!(key in env)) delete process.env[key];
      Object.assign(process.env, env);
      await rm(directory, { recursive: true, force: true });
    },
  };
}

test("the real extension applies routing, persists overrides, and keeps explicit spawn precedence", async () => {
  const h = await harness();
  try {
    await h.command("subagent-model");
    assert.match(h.notices.at(-1)!, /gpt-6\.1-sol \(rule\)/);
    await h.command("subagent-model", "claude-bridge/claude-opus-5");
    const overridden = await h.tool("spawn_sub_agent", { task: "finite work" });
    assert.equal(
      overridden.details.requestedModel,
      "claude-bridge/claude-opus-5",
    );
    const explicit = await h.tool("spawn_sub_agent", {
      task: "finite work",
      model: "openai-codex/gpt-6-sol:high",
    });
    assert.equal(
      explicit.details.requestedModel,
      "openai-codex/gpt-6-sol:high",
    );
    assert.equal(explicit.details.modelSource, "explicit");
    await h.command("subagent-model", "auto");
    const automatic = await h.tool("spawn_sub_agent", { task: "finite work" });
    assert.equal(automatic.details.requestedModel, "openai-codex/gpt-6.1-sol");
    assert.equal(h.ctx.model.id, "gpt-6-astra");
    assert.equal(h.branch.at(-1)!.data.model, null);
  } finally {
    await h.close();
  }
});

test("model changes are published to Paj rather than leaving launch metadata stale", async () => {
  const h = await harness();
  try {
    await h.event("model_select", {
      model: { provider: "claude-bridge", id: "claude-opus-5" },
    });
    assert.ok(
      h.calls.some(
        (call) =>
          call.args.join(" ") ===
          "session model session claude-bridge/claude-opus-5",
      ),
    );
  } finally {
    await h.close();
  }
});

test("the polling integration queues nothing while busy and injects one batch when idle", async () => {
  const h = await harness({ busy: true });
  try {
    h.setInbox(
      Array.from({ length: 12 }, (_, i) => ({
        id: String(i),
        from: { name: "child" },
        kind: "result",
        text: "done",
      })),
    );
    await h.poll();
    assert.equal(h.messages.length, 0);
    assert.equal(h.inbox().length, 12);
    h.setIdle(true);
    await h.poll();
    assert.equal(h.messages.length, 1);
    assert.equal(h.messages[0].details.messageIds.length, 12);
    assert.equal(h.messages[0].delivery.triggerTurn, true);
    assert.equal(h.inbox().length, 0);
  } finally {
    await h.close();
  }
});

test("completion closes the assignment and late questions are archived without another turn", async () => {
  const h = await harness({ child: true });
  try {
    await h.event("before_agent_start", {
      systemPrompt: "system",
      messageType: "user",
    });
    await assert.rejects(
      h.tool("complete_subagent_task", { taskId: "wrong", summary: "done" }),
      /does not match/,
    );
    const result = await h.tool("complete_subagent_task", {
      taskId: "task",
      summary: "done",
    });
    assert.equal(result.terminate, true);
    h.setInbox([
      {
        id: "late",
        taskId: "task",
        kind: "question",
        from: { name: "parent" },
        text: "status?",
      },
    ]);
    await h.poll();
    assert.equal(h.messages.length, 0);
    assert.equal(h.inbox().length, 0);
    assert.equal(h.branch.at(-1)!.customType, "paj-stale-messages");
  } finally {
    await h.close();
  }
});

test("updates are persisted without waking Pi or using a volatile next-turn queue", async () => {
  const h = await harness();
  try {
    h.setInbox([
      {
        id: "info",
        kind: "update",
        from: { name: "other" },
        text: "relevant finding",
      },
    ]);
    await h.poll();
    assert.equal(h.messages[0].delivery.triggerTurn, false);
    assert.equal(h.messages[0].delivery.deliverAs, undefined);
    assert.equal(h.branch.at(-1)!.type, "custom_message");
  } finally {
    await h.close();
  }
});

test("reload preserves the registration, pending inbox, and session override", async () => {
  const h = await harness({ busy: true });
  try {
    await h.command("subagent-model", "claude-bridge/claude-opus-5");
    h.setInbox([
      { id: "pending", kind: "result", from: { name: "child" }, text: "done" },
    ]);
    await h.reload();
    assert.equal(
      h.calls.filter((call) => call.args.includes("register")).length,
      1,
    );
    assert.equal(
      h.calls.filter((call) => call.args.includes("unregister")).length,
      0,
    );
    assert.equal(h.inbox().length, 1);
    await h.command("subagent-model");
    assert.match(h.notices.at(-1)!, /claude-opus-5 \(session\)/);
    h.setIdle(true);
    await h.poll();
    assert.equal(h.messages.length, 1);
  } finally {
    await h.close();
  }
});

test("a saved completion retries its outbox without waking the completed child", async () => {
  const h = await harness({ child: true });
  try {
    h.setRecord({ taskState: "completed", result: { delivered: false } });
    await h.poll();
    assert.ok(
      h.calls.some(
        (call) => call.args.join(" ") === "--json subagent flush spawn",
      ),
    );
    assert.equal(h.messages.length, 0);
  } finally {
    await h.close();
  }
});

test("nested parents accept current children's reports but archive previous-assignment reports", async () => {
  const h = await harness({ child: true });
  try {
    h.setChildren([
      {
        spawnId: "nested",
        taskId: "nested-task",
        parentTaskId: "task",
        resultHistory: [
          { taskId: "earlier-subtask", parentTaskId: "task", delivered: true },
          {
            taskId: "previous-assignment-subtask",
            parentTaskId: "old-task",
            delivered: true,
          },
        ],
      },
      {
        spawnId: "old-nested",
        taskId: "old-nested-task",
        parentTaskId: "old-task",
      },
    ]);
    h.setInbox([
      {
        id: "current",
        taskId: "nested-task",
        kind: "result",
        from: { name: "nested" },
        text: "done",
      },
      {
        id: "old",
        taskId: "old-nested-task",
        kind: "result",
        from: { name: "old-nested" },
        text: "old",
      },
      {
        id: "earlier",
        taskId: "earlier-subtask",
        kind: "result",
        from: { name: "nested" },
        text: "earlier result",
      },
      {
        id: "previous",
        taskId: "previous-assignment-subtask",
        kind: "result",
        from: { name: "nested" },
        text: "previous assignment",
      },
    ]);
    await h.poll();
    assert.equal(h.messages.length, 1);
    assert.deepEqual(h.messages[0].details.messageIds, ["current", "earlier"]);
    assert.ok(
      h.branch.some(
        (entry) =>
          entry.customType === "paj-stale-messages" &&
          entry.data.messageIds.includes("old"),
      ),
    );
  } finally {
    await h.close();
  }
});

test("sibling answers reach the current assignment without changing its completion identity", async () => {
  const h = await harness({ child: true });
  try {
    const answer = {
      id: "answer",
      taskId: "sibling-task",
      senderTaskId: "sibling-task",
      recipientTaskId: "task",
      kind: "answer",
      inReplyTo: "question",
      from: { name: "sibling" },
      text: "A JSON object",
    };
    h.setInbox([answer]);
    await h.poll();
    assert.equal(h.messages.length, 1);
    assert.match(h.messages[0].content, /recipient assignment task/);
    await h.event("before_agent_start", {
      systemPrompt: "system",
      messageType: "custom",
    });
    const completed = await h.tool("complete_subagent_task", {
      taskId: "task",
      summary: "done",
    });
    assert.equal(completed.details.taskState, "completed");
    h.setRecord({ taskId: "next-task", taskState: "active" });
    h.setIdle(true);
    h.setInbox([{ ...answer, id: "late-answer" }]);
    await h.poll();
    assert.equal(h.messages.length, 1);
    assert.ok(
      h.branch.some(
        (entry) =>
          entry.customType === "paj-stale-messages" &&
          entry.data.messageIds.includes("late-answer"),
      ),
    );
  } finally {
    await h.close();
  }
});

test("a new assignment reaches a completed child with a new task identity", async () => {
  const h = await harness({ child: true });
  try {
    h.setRecord({ taskId: "next-task", taskState: "active" });
    h.setInbox([
      {
        id: "old",
        taskId: "task",
        kind: "question",
        from: { name: "sibling" },
        text: "old",
      },
      {
        id: "next",
        taskId: "next-task",
        kind: "request",
        from: { name: "parent" },
        text: "new work",
      },
    ]);
    await h.poll();
    assert.equal(h.messages.length, 1);
    assert.deepEqual(h.messages[0].details.messageIds, ["next"]);
    await h.event("before_agent_start", {
      systemPrompt: "system",
      messageType: "custom",
    });
    const result = await h.tool("complete_subagent_task", {
      taskId: "next-task",
      summary: "done",
    });
    assert.equal(result.details.taskState, "completed");
  } finally {
    await h.close();
  }
});
