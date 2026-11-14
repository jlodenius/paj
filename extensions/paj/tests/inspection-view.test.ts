import assert from "node:assert/strict";
import test from "node:test";
import { createInspectionView } from "../inspection-view.ts";
import { MAX_INSPECTION_BYTES, MAX_INSPECTION_TEXT_BYTES } from "../inspection-types.ts";

const identity = { id: "paj-test", pid: 123, piSessionId: "pi-test", role: "main" };
const instant = 1_700_000_000_000;

function harness() {
  let branch: unknown[] = [];
  let idle = false;
  const view = createInspectionView({ now: () => instant });
  const ctx = {
    isIdle: () => idle,
    sessionManager: {
      getSessionId: () => identity.piSessionId,
      getBranch: () => branch,
    },
  };
  return {
    view,
    ctx,
    setBranch: (value: unknown[]) => { branch = value; },
    setIdle: (value: boolean) => { idle = value; },
    snapshot: (limit?: number) => view.snapshot(identity, ctx, limit),
  };
}

function message(id: string, role: string, content: unknown, extra = {}) {
  return { id, type: "message", timestamp: instant, message: { role, content, ...extra } };
}

function assistant(text: string, extra = {}) {
  return { role: "assistant", content: [{ type: "text", text }], timestamp: instant, ...extra };
}

test("projects only visible active-branch messages without mutation or extra context calls", () => {
  const h = harness();
  const branch = [
    message("user", "user", "hello"),
    message("assistant", "assistant", [
      { type: "thinking", thinking: "PRIVATE_REASONING" },
      { type: "text", text: "visible answer" },
      { type: "toolCall", name: "bash", arguments: { command: "PRIVATE_ARGS" } },
    ]),
    message("system", "system", "PRIVATE_SYSTEM"),
    { id: "visible", type: "custom_message", display: true, content: "visible custom" },
    { id: "hidden", type: "custom_message", display: false, content: "PRIVATE_CUSTOM" },
    { id: "config", type: "custom", data: "PRIVATE_ENV" },
    message("tool", "toolResult", "PRIVATE_RESULT", { toolName: "bash", isError: false }),
    message("failed", "toolResult", "PRIVATE_ERROR", { toolName: "read", isError: true }),
  ];
  const before = structuredClone(branch);
  h.setBranch(branch);
  const snapshot = h.snapshot();
  assert.deepEqual(snapshot.history.map(({ role, text }) => ({ role, text })), [
    { role: "user", text: "hello" },
    { role: "assistant", text: "visible answer" },
    { role: "user", text: "visible custom" },
    { role: "tool", text: "bash: completed" },
    { role: "tool", text: "read: failed" },
  ]);
  assert.doesNotMatch(JSON.stringify(snapshot), /PRIVATE/);
  assert.deepEqual(branch, before);
  h.setBranch([message("other", "user", "new active branch")]);
  assert.deepEqual(h.snapshot().history.map((item) => item.id), ["other"]);
  h.setIdle(true);
  assert.equal(h.snapshot().status, "idle");
  assert.equal(snapshot.status, "busy");
});

test("projects partial assistant updates and retains the final overlay until persistence", () => {
  const h = harness();
  h.view.observe({ type: "message_start", message: assistant("") });
  h.view.observe({ type: "message_update", message: assistant("partial", {
    privateField: "PRIVATE",
    content: [{ type: "thinking", thinking: "PRIVATE" }, { type: "text", text: "partial" }],
  }), assistantMessageEvent: { type: "thinking_delta", delta: "PRIVATE" } });
  assert.deepEqual(h.snapshot().liveAssistant, { text: "partial", streaming: true });
  h.view.observe({ type: "message_end", message: assistant("final") });
  assert.deepEqual(h.snapshot().liveAssistant, { text: "final", streaming: false });
  assert.deepEqual(h.snapshot().liveAssistant, { text: "final", streaming: false });
  h.setBranch([message("persisted", "assistant", assistant("final").content)]);
  assert.equal(h.snapshot().liveAssistant, null);
  assert.equal(h.snapshot().history[0].text, "final");
  assert.doesNotMatch(JSON.stringify(h.snapshot()), /PRIVATE/);
});

test("an earlier identical answer does not prematurely consume a final without a timestamp", () => {
  const h = harness();
  h.setBranch([message("previous", "assistant", "same answer")]);
  h.snapshot();
  h.view.observe({ type: "message_end", message: { role: "assistant", content: "same answer" } });
  assert.deepEqual(h.snapshot().liveAssistant, { text: "same answer", streaming: false });
  h.setBranch([
    message("previous", "assistant", "same answer"),
    message("new", "assistant", "same answer"),
  ]);
  assert.equal(h.snapshot().liveAssistant, null);
});

test("observations keep safe copies rather than references to event content", () => {
  const h = harness();
  const event = { type: "message_update", message: assistant("original") };
  h.view.observe(event);
  event.message.content[0].text = "mutated";
  assert.equal(h.snapshot().liveAssistant?.text, "original");
  const snapshot = h.snapshot();
  snapshot.liveAssistant!.text = "also mutated";
  assert.equal(h.snapshot().liveAssistant?.text, "original");
});

test("does not replace assistant overlay with user messages or tool results", () => {
  const h = harness();
  h.view.observe({ type: "message_update", message: assistant("visible") });
  h.view.observe({ type: "message_end", message: { role: "toolResult", content: "PRIVATE" } });
  h.view.observe({ type: "message_start", message: { role: "user", content: "hello" } });
  assert.equal(h.snapshot().liveAssistant?.text, "visible");
});

test("tool activity contains no args/results and update does not infer completion", () => {
  const h = harness();
  h.view.observe({ type: "tool_execution_start", toolCallId: "call-1", toolName: "bash", args: { command: "PRIVATE" } });
  h.view.observe({ type: "tool_execution_update", toolCallId: "call-1", partialResult: "PRIVATE", isError: true });
  const snapshot = h.snapshot();
  assert.deepEqual(snapshot.activeTools, [{ toolCallId: "call-1", toolName: "bash", startedAt: new Date(instant).toISOString() }]);
  assert.equal(snapshot.lastActivityAt, new Date(instant).toISOString());
  assert.doesNotMatch(JSON.stringify(snapshot), /PRIVATE/);
  snapshot.activeTools[0].toolName = "mutated";
  assert.equal(h.snapshot().activeTools[0].toolName, "bash");
  h.view.observe({ type: "tool_execution_end", toolCallId: "call-1", result: "PRIVATE" });
  assert.deepEqual(h.snapshot().activeTools, []);
  h.view.observe({ type: "tool_execution_start", toolCallId: "unsafe", toolName: "bash\nPRIVATE" });
  assert.equal(h.snapshot().activeTools[0].toolName, "unknown");
});

test("active tools are bounded to 32 and overflow is signalled", () => {
  const h = harness();
  for (let i = 0; i < 40; i++) {
    h.view.observe({ type: "tool_execution_start", toolCallId: `call-${i}`, toolName: "read" });
  }
  assert.equal(h.snapshot().activeTools.length, 32);
  assert.equal(h.snapshot().activeTools[0].toolCallId, "call-8");
  assert.equal(h.snapshot().truncated, true);
});

test("serialized Unicode, escaped text, images and whole snapshot remain bounded", () => {
  const h = harness();
  const text = "😀\u0000\n\\\"界".repeat(3000);
  const branch = Array.from({ length: 80 }, (_, i) => message(`entry-${i}`, "user", [
    { type: "text", text },
    { type: "image", data: "PRIVATE_IMAGE_BYTES".repeat(1000), mimeType: "image/png" },
  ]));
  h.setBranch(branch);
  h.view.observe({ type: "message_update", message: assistant(text) });
  const snapshot = h.snapshot(50);
  assert.ok(Buffer.byteLength(JSON.stringify(snapshot)) < MAX_INSPECTION_BYTES);
  assert.ok(snapshot.history.length < 50);
  assert.equal(snapshot.history.at(-1)?.id, "entry-79");
  assert.equal(snapshot.truncated, true);
  for (const item of snapshot.history) {
    assert.equal(item.truncated, true);
    assert.ok(Buffer.byteLength(JSON.stringify(item.text)) <= MAX_INSPECTION_TEXT_BYTES);
    assert.doesNotMatch(item.text, /[\ud800-\udbff]$/u);
  }
  assert.equal(snapshot.liveAssistant?.truncated, true);
  assert.doesNotMatch(JSON.stringify(snapshot), /PRIVATE_IMAGE_BYTES/);
  h.setBranch([message("image", "user", [{ type: "image", data: "PRIVATE_IMAGE_BYTES" }])]);
  assert.equal(h.snapshot().history[0].text, "[image]");
});

test("history limit prefers newest and signals omissions, defaulting to 20", () => {
  const h = harness();
  h.setBranch(Array.from({ length: 25 }, (_, i) => message(`${i}`, "user", "hello")));
  assert.equal(h.snapshot().history.length, 20);
  assert.deepEqual(h.snapshot(2).history.map((item) => item.id), ["23", "24"]);
  assert.equal(h.snapshot(2).truncated, true);
  h.setBranch([message("only", "user", "hello")]);
  assert.equal(h.snapshot().truncated, false);
});

test("branch, compact, settled and reset clean transient state", () => {
  for (const type of ["session_tree", "session_compact", "agent_settled", "reset"]) {
    const h = harness();
    h.view.observe({ type: "message_end", message: assistant("unpersisted") });
    h.view.observe({ type: "tool_execution_start", toolCallId: "call", toolName: "read" });
    if (type === "reset") h.view.reset();
    else h.view.observe({ type });
    assert.equal(h.snapshot().liveAssistant, null, type);
    assert.deepEqual(h.snapshot().activeTools, [], type);
    assert.equal(h.snapshot().lastActivityAt, type === "agent_settled" ? new Date(instant).toISOString() : null, type);
  }
});

test("rejects invalid limits and stale session identity without reading the branch", () => {
  const h = harness();
  for (const value of [0, -1, 51, 1.5, NaN, Infinity, "2", null]) {
    assert.throws(() => h.snapshot(value as number), /limit/);
  }
  assert.throws(() => h.view.snapshot({ ...identity, piSessionId: "stale" }, {
    ...h.ctx,
    sessionManager: {
      getSessionId: () => identity.piSessionId,
      getBranch: () => { throw new Error("must not read stale branch"); },
    },
  }), /identity changed/);
  assert.throws(() => h.view.snapshot({ ...identity, name: "x".repeat(10000) }, h.ctx), /Invalid inspection identity/);
});

test("malformed entries, invalid dates and unknown events do not expose private data", () => {
  const h = harness();
  h.setBranch([null, 42, {}, { type: "custom_message", content: "PRIVATE" },
    message("date", "user", "visible", { timestamp: "not a date" }),
    message("unknown", "environment", "PRIVATE"),
  ]);
  h.view.observe({ type: "unknown", data: "PRIVATE" });
  h.view.observe({ type: "tool_execution_update", toolCallId: "not-started", partialResult: "PRIVATE" });
  const snapshot = h.snapshot();
  assert.equal(snapshot.history.length, 1);
  assert.equal(snapshot.history[0].timestamp, undefined);
  assert.deepEqual(snapshot.activeTools, []);
  assert.doesNotMatch(JSON.stringify(snapshot), /PRIVATE/);
});
