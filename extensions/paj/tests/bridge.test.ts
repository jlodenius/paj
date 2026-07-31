import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { connect, type Socket } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { BridgeServer, type BridgeActions } from "../bridge.ts";

const REQUEST_ID = "019fa92e-a7c2-7072-84a7-8933262464a5";

async function withServer(
  actions: BridgeActions,
  run: (socketPath: string, server: BridgeServer) => Promise<void>,
) {
  const directory = await mkdtemp(join(tmpdir(), "paj-bridge-test-"));
  const socketPath = join(directory, "bridge.sock");
  const server = new BridgeServer(actions);
  await server.start(socketPath);
  try {
    await run(socketPath, server);
  } finally {
    await server.stop();
    await rm(directory, { recursive: true, force: true });
  }
}

function open(socketPath: string): Promise<Socket> {
  return new Promise((resolve, reject) => {
    const socket = connect(socketPath, () => resolve(socket));
    socket.once("error", reject);
  });
}

function collect(socket: Socket): Promise<Record<string, unknown>[]> {
  return new Promise((resolve) => {
    let output = "";
    socket.on("data", (chunk) => {
      output += chunk.toString("utf8");
    });
    socket.on("error", () => undefined);
    socket.on("close", () => {
      resolve(
        output
          .split("\n")
          .filter(Boolean)
          .map((line) => JSON.parse(line) as Record<string, unknown>),
      );
    });
  });
}

function request(overrides: Record<string, unknown> = {}) {
  return JSON.stringify({
    version: 1,
    id: REQUEST_ID,
    method: "prompt",
    params: { text: "hello" },
    ...overrides,
  });
}

function actions(overrides: Partial<BridgeActions> = {}) {
  return {
    isIdle: () => true,
    sendPrompt: () => undefined,
    cancelPrompt: () => undefined,
    ...overrides,
  };
}

test("streams a correlated response without cancelling completed work", async () => {
  const prompts: string[] = [];
  let cancellations = 0;
  await withServer(
    actions({
      sendPrompt: (text) => prompts.push(text),
      cancelPrompt: () => cancellations++,
    }),
    async (socketPath, server) => {
      const socket = await open(socketPath);
      const eventsPromise = collect(socket);
      socket.write(request() + "\n");
      await new Promise<void>((resolve) => socket.once("data", () => resolve()));
      server.onMessageUpdate({
        assistantMessageEvent: { type: "text_delta", delta: "hel" },
      });
      server.onMessageEnd({
        message: {
          role: "assistant",
          content: [{ type: "text", text: "hello" }],
        },
      });
      server.onAgentSettled();

      const events = await eventsPromise;
      assert.deepEqual(
        events.map((event) => event.event),
        ["accepted", "delta", "complete"],
      );
      assert.deepEqual(prompts, ["hello"]);
      assert.deepEqual(events.at(-1)?.actions, []);
      assert.equal(cancellations, 0);
    },
  );
});

test("emits validated proposals atomically with generated unique IDs", async () => {
  await withServer(actions(), async (socketPath, server) => {
    const socket = await open(socketPath);
    const eventsPromise = collect(socket);
    socket.write(request() + "\n");
    await new Promise<void>((resolve) => socket.once("data", () => resolve()));
    server.onMessageUpdate({
      assistantMessageEvent: { type: "text_delta", delta: "Consider these." },
    });
    server.onMessageEnd({
      message: {
        role: "assistant",
        content: [{ type: "text", text: "Consider these." }],
      },
    });
    const submitted = server.submitProposals([
      { title: "First", description: "Implement the first change." },
      { title: "Second", description: "Implement the second change." },
    ]);
    assert.equal(submitted.length, 2);
    assert.notEqual(submitted[0]?.id, submitted[1]?.id);
    for (const action of submitted) {
      assert.match(action.id, /^[0-9a-f-]{36}$/i);
      assert.notEqual(action.id, action.title);
    }
    server.onAgentSettled();

    const events = await eventsPromise;
    assert.deepEqual(events.map((event) => event.event), [
      "accepted",
      "delta",
      "complete",
    ]);
    assert.deepEqual(events.at(-1)?.actions, submitted);
  });
});

test("rejects proposal misuse and invalid fields", async (t) => {
  const inactive = new BridgeServer(actions());
  assert.throws(() => inactive.submitProposals([]), /active bridge request/);

  for (const [name, proposals, message] of [
    ["not an array", {}, "must be an array"],
    ["empty", [], "at least one action"],
    [
      "too many",
      Array.from({ length: 21 }, () => ({ title: "t", description: "d" })),
      "at most 20",
    ],
    ["empty title", [{ title: " ", description: "d" }], "title is required"],
    [
      "long title",
      [{ title: "é".repeat(101), description: "d" }],
      "title exceeds 200 bytes",
    ],
    [
      "empty description",
      [{ title: "t", description: "" }],
      "description is required",
    ],
    [
      "long description",
      [{ title: "t", description: "é".repeat(2001) }],
      "description exceeds 4000 bytes",
    ],
    [
      "extra field",
      [{ title: "t", description: "d", id: "model-id" }],
      "is invalid",
    ],
  ] as const) {
    await t.test(name, async () => {
      await withServer(actions(), async (socketPath, server) => {
        const socket = await open(socketPath);
        const eventsPromise = collect(socket);
        socket.write(request() + "\n");
        await new Promise<void>((resolve) =>
          socket.once("data", () => resolve()),
        );
        assert.throws(
          () => server.submitProposals(proposals),
          new RegExp(message),
        );
        server.onAgentSettled();
        const events = await eventsPromise;
        assert.deepEqual(events.at(-1)?.actions, []);
      });
    });
  }
});

test("rejects repeated proposal calls", async () => {
  await withServer(actions(), async (socketPath, server) => {
    const socket = await open(socketPath);
    const eventsPromise = collect(socket);
    socket.write(request() + "\n");
    await new Promise<void>((resolve) => socket.once("data", () => resolve()));
    const proposal = [{ title: "Title", description: "Description" }];
    server.submitProposals(proposal);
    assert.throws(
      () => server.submitProposals(proposal),
      /only be called once/,
    );
    server.onAgentSettled();
    await eventsPromise;
  });
});

test("activates and cleans up the proposal tool across lifecycle outcomes", async (t) => {
  await t.test("settled", async () => {
    const states: boolean[] = [];
    await withServer(
      actions({ setProposalToolActive: (active) => states.push(active) }),
      async (socketPath, server) => {
        const socket = await open(socketPath);
        const eventsPromise = collect(socket);
        socket.write(request() + "\n");
        await new Promise<void>((resolve) => socket.once("data", () => resolve()));
        server.onAgentSettled();
        await eventsPromise;
      },
    );
    assert.deepEqual(states, [true, false]);
  });

  await t.test("prompt failure", async () => {
    const states: boolean[] = [];
    await withServer(
      actions({
        setProposalToolActive: (active) => states.push(active),
        sendPrompt: () => {
          throw new Error("failed");
        },
      }),
      async (socketPath) => {
        const socket = await open(socketPath);
        const eventsPromise = collect(socket);
        socket.write(request() + "\n");
        await eventsPromise;
      },
    );
    assert.deepEqual(states, [true, false]);
  });
});

test("cancels exactly once when an accepted client disconnects", async () => {
  let cancellations = 0;
  const states: boolean[] = [];
  await withServer(
    actions({
      cancelPrompt: () => cancellations++,
      setProposalToolActive: (active) => states.push(active),
    }),
    async (socketPath) => {
      const socket = await open(socketPath);
      socket.write(request() + "\n");
      await new Promise<void>((resolve) => socket.once("data", () => resolve()));
      const closed = new Promise<void>((resolve) =>
        socket.once("close", () => resolve()),
      );
      socket.destroy();
      await closed;
      for (let attempt = 0; attempt < 50 && cancellations === 0; attempt++) {
        await new Promise((resolve) => setTimeout(resolve, 2));
      }
      assert.equal(cancellations, 1);
    },
  );
  assert.equal(cancellations, 1);
  assert.deepEqual(states, [true, false]);
});

test("rejects null, arrays, malformed JSON, and malformed IDs without dispatch", async (t) => {
  for (const [name, line] of [
    ["null", "null"],
    ["array", "[]"],
    ["malformed", "{"],
    ["malformed ID", request({ id: "not-a-uuid" })],
  ]) {
    await t.test(name, async () => {
      let dispatched = false;
      await withServer(
        actions({ sendPrompt: () => (dispatched = true) }),
        async (socketPath) => {
          const socket = await open(socketPath);
          const eventsPromise = collect(socket);
          socket.end(line + "\n");
          assert.deepEqual(await eventsPromise, []);
          assert.equal(dispatched, false);
        },
      );
    });
  }
});

test("returns correlated errors for version, method, and empty prompt mismatches", async (t) => {
  for (const [name, line, message] of [
    ["version", request({ version: 2 }), "unsupported bridge protocol version 2"],
    ["method", request({ method: "cancel" }), "unsupported bridge method cancel"],
    ["empty prompt", request({ params: { text: "  " } }), "prompt text is required"],
    ["array params", request({ params: [] }), "prompt text is required"],
  ]) {
    await t.test(name, async () => {
      await withServer(actions(), async (socketPath) => {
        const socket = await open(socketPath);
        const eventsPromise = collect(socket);
        socket.end(line + "\n");
        const [event] = await eventsPromise;
        assert.equal(event.event, "error");
        assert.equal(event.id, REQUEST_ID);
        assert.equal(event.code, "invalid_request");
        assert.equal(event.message, message);
      });
    });
  }
});

test("rejects oversized requests before dispatch", async () => {
  let dispatched = false;
  await withServer(
    actions({ sendPrompt: () => (dispatched = true) }),
    async (socketPath) => {
      const socket = await open(socketPath);
      const eventsPromise = collect(socket);
      socket.end(Buffer.alloc(1024 * 1024 + 1, 0x61));
      assert.deepEqual(await eventsPromise, []);
      assert.equal(dispatched, false);
    },
  );
});

test("rejects requests while Pi is busy without cancelling", async () => {
  let cancellations = 0;
  await withServer(
    actions({
      isIdle: () => false,
      cancelPrompt: () => cancellations++,
    }),
    async (socketPath) => {
      const socket = await open(socketPath);
      const eventsPromise = collect(socket);
      socket.end(request() + "\n");
      const [event] = await eventsPromise;
      assert.equal(event.event, "error");
      assert.equal(event.code, "busy");
    },
  );
  assert.equal(cancellations, 0);
});

test("a rejected secondary client does not cancel the active request", async () => {
  let cancellations = 0;
  await withServer(
    actions({ cancelPrompt: () => cancellations++ }),
    async (socketPath, server) => {
      const active = await open(socketPath);
      const activeEvents = collect(active);
      active.write(request() + "\n");
      await new Promise<void>((resolve) =>
        active.once("data", () => resolve()),
      );

      const secondary = await open(socketPath);
      const secondaryEvents = collect(secondary);
      secondary.end(
        request({ id: "019faa4d-63ce-72a2-9d5a-c870d33ecfcb" }) + "\n",
      );
      const [rejection] = await secondaryEvents;
      assert.equal(rejection.code, "busy");
      assert.equal(cancellations, 0);

      server.onAgentSettled();
      const events = await activeEvents;
      assert.equal(events.at(-1)?.event, "complete");
    },
  );
  assert.equal(cancellations, 0);
});

test("server shutdown does not cancel an active request", async () => {
  let cancellations = 0;
  const directory = await mkdtemp(join(tmpdir(), "paj-bridge-test-"));
  const socketPath = join(directory, "bridge.sock");
  const server = new BridgeServer(
    actions({ cancelPrompt: () => cancellations++ }),
  );
  await server.start(socketPath);
  const socket = await open(socketPath);
  const events = collect(socket);
  socket.write(request() + "\n");
  await new Promise<void>((resolve) => socket.once("data", () => resolve()));

  await server.stop();
  const response = await events;
  await rm(directory, { recursive: true, force: true });

  assert.equal(response.at(-1)?.code, "shutting_down");
  assert.equal(cancellations, 0);
});
