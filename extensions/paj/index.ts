import type {
  ExtensionAPI,
  ExtensionContext,
} from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

import { BridgeServer } from "./bridge.ts";

interface PajMessage {
  id: string;
  from: {
    name: string;
  };
  text: string;
}

interface PajSession {
  id: string;
  name: string;
  pid: number;
  piSessionId?: string;
  projectRoot: string;
  branch?: string;
  role: string;
  task?: string;
  status: string;
  bridgeSocket?: string;
}

const COMMAND_TIMEOUT_MS = 5_000;
const HEARTBEAT_INTERVAL_MS = 10_000;
const MESSAGE_POLL_INTERVAL_MS = 1_000;
const REGISTRATION_RETRY_INITIAL_MS = 1_000;
const REGISTRATION_RETRY_MAX_MS = 30_000;

export default function pajExtension(pi: ExtensionAPI) {
  let activeSessionId: string | undefined;
  let heartbeatTimer: ReturnType<typeof setInterval> | undefined;
  let heartbeatPending: Promise<void> | undefined;
  let messageTimer: ReturnType<typeof setInterval> | undefined;
  let messagePollPending: Promise<void> | undefined;
  let bridge: BridgeServer | undefined;
  let registrationPending: Promise<void> | undefined;
  let registrationRetryTimer: ReturnType<typeof setTimeout> | undefined;
  let registrationRetryMs = REGISTRATION_RETRY_INITIAL_MS;
  let registeredName: string | undefined;
  let lastMessagePollError: string | undefined;
  let shuttingDown = false;

  const execPaj = async (args: string[], cwd: string) => {
    const result = await pi.exec("paj", args, {
      cwd,
      timeout: COMMAND_TIMEOUT_MS,
    });
    if (result.code !== 0) {
      throw new Error(
        result.stderr.trim() || `paj exited with code ${result.code}`,
      );
    }
    return result.stdout;
  };

  const sendToAgent = async (recipient: string, text: string, cwd: string) => {
    if (!activeSessionId) {
      throw new Error("paj session is not registered");
    }
    const output = await execPaj(
      [
        "--json",
        "message",
        "send",
        recipient,
        "--from",
        activeSessionId,
        "--text",
        text,
      ],
      cwd,
    );
    return JSON.parse(output) as PajMessage;
  };

  const pollMessages = async (ctx: ExtensionContext, sessionId: string) => {
    const output = await execPaj(
      ["--json", "message", "pending", sessionId],
      ctx.cwd,
    );
    const messages = JSON.parse(output) as PajMessage[];
    let deliverImmediately = ctx.isIdle();
    for (const message of messages) {
      const content = `[Message from ${message.from.name}]\n${message.text}`;
      if (deliverImmediately) {
        pi.sendUserMessage(content);
        deliverImmediately = false;
      } else {
        pi.sendUserMessage(content, { deliverAs: "followUp" });
      }
      await execPaj(["message", "ack", sessionId, message.id], ctx.cwd);
    }
  };

  const isMissingSessionError = (error: unknown) =>
    /session [0-9a-f-]+ was not found/.test(String(error));

  const setDisconnected = (ctx: ExtensionContext) => {
    if (ctx.hasUI) {
      ctx.ui.setStatus("paj", ctx.ui.theme.fg("error", "paj:disconnected"));
    }
  };

  const teardownRegistration = async (ctx: ExtensionContext) => {
    const previousBridge = bridge;
    const previousSessionId = activeSessionId;
    bridge = undefined;
    activeSessionId = undefined;
    if (previousBridge) {
      await previousBridge
        .stop()
        .catch((error: unknown) =>
          console.error("paj bridge shutdown failed", error),
        );
    }
    if (previousSessionId) {
      await execPaj(
        ["session", "unregister", previousSessionId],
        ctx.cwd,
      ).catch((error: unknown) => {
        if (!isMissingSessionError(error)) {
          console.error("paj unregister failed", error);
        }
      });
    }
  };

  const registerSession = async (ctx: ExtensionContext) => {
    const args = [
      "--json",
      "session",
      "register",
      "--pid",
      String(process.pid),
      "--pi-session-id",
      ctx.sessionManager.getSessionId(),
      "--role",
      process.env.PAJ_ROLE ?? "primary",
      "--cwd",
      ctx.cwd,
    ];
    const name =
      pi.getSessionName() ?? process.env.PAJ_AGENT_NAME ?? registeredName;
    if (name) {
      args.push("--name", name);
    }
    const task = process.env.PAJ_TASK;
    if (task) {
      args.push("--task", task);
    }

    const output = await execPaj(args, ctx.cwd);
    const session = JSON.parse(output) as PajSession;
    if (!session.bridgeSocket) {
      await execPaj(["session", "unregister", session.id], ctx.cwd).catch(
        () => undefined,
      );
      throw new Error("paj session did not advertise a bridge socket");
    }
    const nextBridge = new BridgeServer({
      isIdle: () => ctx.isIdle(),
      sendPrompt: (text) => pi.sendUserMessage(text),
    });
    try {
      await nextBridge.start(session.bridgeSocket);
    } catch (error) {
      await execPaj(["session", "unregister", session.id], ctx.cwd).catch(
        () => undefined,
      );
      throw error;
    }
    bridge = nextBridge;
    activeSessionId = session.id;
    registeredName = session.name;
  };

  const scheduleRegistrationRetry = (ctx: ExtensionContext) => {
    if (shuttingDown || registrationRetryTimer) {
      return;
    }
    const delay = registrationRetryMs;
    registrationRetryMs = Math.min(
      registrationRetryMs * 2,
      REGISTRATION_RETRY_MAX_MS,
    );
    registrationRetryTimer = setTimeout(() => {
      registrationRetryTimer = undefined;
      void beginRegistration(ctx).catch(() => undefined);
    }, delay);
  };

  const beginRegistration = (ctx: ExtensionContext): Promise<void> => {
    if (registrationPending) {
      return registrationPending;
    }
    if (registrationRetryTimer) {
      clearTimeout(registrationRetryTimer);
      registrationRetryTimer = undefined;
    }
    const attempt = (async () => {
      await teardownRegistration(ctx);
      if (shuttingDown) {
        return;
      }
      await registerSession(ctx);
    })();
    registrationPending = attempt;
    void attempt.then(
      () => {
        registrationRetryMs = REGISTRATION_RETRY_INITIAL_MS;
        if (ctx.hasUI) {
          ctx.ui.setStatus("paj", undefined);
        }
        if (registrationPending === attempt) {
          registrationPending = undefined;
        }
      },
      (error: unknown) => {
        setDisconnected(ctx);
        console.error("paj registration failed; retrying", error);
        if (registrationPending === attempt) {
          registrationPending = undefined;
        }
        scheduleRegistrationRetry(ctx);
      },
    );
    return attempt;
  };

  const handleOperationalFailure = (
    ctx: ExtensionContext,
    operation: "heartbeat" | "message poll",
    sessionId: string,
    error: unknown,
  ) => {
    if (sessionId !== activeSessionId) {
      return;
    }
    if (isMissingSessionError(error)) {
      setDisconnected(ctx);
      console.error(`paj ${operation} lost registration; reconnecting`);
      void beginRegistration(ctx).catch(() => undefined);
      return;
    }
    setDisconnected(ctx);
    const message = String(error);
    if (operation !== "message poll" || message !== lastMessagePollError) {
      console.error(`paj ${operation} failed`, error);
    }
    if (operation === "message poll") {
      lastMessagePollError = message;
    }
  };

  pi.on("session_start", async (_event, ctx) => {
    shuttingDown = false;
    heartbeatTimer = setInterval(() => {
      const sessionId = activeSessionId;
      if (!sessionId || heartbeatPending) {
        return;
      }
      heartbeatPending = execPaj(["session", "heartbeat", sessionId], ctx.cwd)
        .then(() => {
          if (sessionId === activeSessionId && ctx.hasUI) {
            ctx.ui.setStatus("paj", undefined);
          }
        })
        .catch((error: unknown) =>
          handleOperationalFailure(ctx, "heartbeat", sessionId, error),
        )
        .finally(() => {
          heartbeatPending = undefined;
        });
    }, HEARTBEAT_INTERVAL_MS);
    const poll = () => {
      const sessionId = activeSessionId;
      if (!sessionId || messagePollPending) {
        return;
      }
      messagePollPending = pollMessages(ctx, sessionId)
        .then(() => {
          if (sessionId === activeSessionId) {
            lastMessagePollError = undefined;
            if (ctx.hasUI) {
              ctx.ui.setStatus("paj", undefined);
            }
          }
        })
        .catch((error: unknown) =>
          handleOperationalFailure(ctx, "message poll", sessionId, error),
        )
        .finally(() => {
          messagePollPending = undefined;
        });
    };
    messageTimer = setInterval(poll, MESSAGE_POLL_INTERVAL_MS);
    await beginRegistration(ctx).catch(() => undefined);
    poll();
  });

  pi.on("message_update", async (event) => {
    bridge?.onMessageUpdate(event);
  });

  pi.on("message_end", async (event) => {
    bridge?.onMessageEnd(event);
  });

  pi.on("agent_settled", async () => {
    bridge?.onAgentSettled();
  });

  pi.on("session_shutdown", async (_event, ctx) => {
    shuttingDown = true;
    if (heartbeatTimer) {
      clearInterval(heartbeatTimer);
      heartbeatTimer = undefined;
    }
    if (messageTimer) {
      clearInterval(messageTimer);
      messageTimer = undefined;
    }
    if (registrationRetryTimer) {
      clearTimeout(registrationRetryTimer);
      registrationRetryTimer = undefined;
    }
    await Promise.all([heartbeatPending, messagePollPending]);
    await registrationPending?.catch(() => undefined);
    await teardownRegistration(ctx);
    if (ctx.hasUI) {
      ctx.ui.setStatus("paj", undefined);
    }
  });

  pi.registerCommand("agent-send", {
    description: "Send a message to another live Pi agent",
    handler: async (args, ctx) => {
      const separator = args.search(/\s/);
      if (separator === -1) {
        ctx.ui.notify("Usage: /agent-send <agent> <message>", "warning");
        return;
      }
      const recipient = args.slice(0, separator);
      const text = args.slice(separator).trim();
      if (!text) {
        ctx.ui.notify("Usage: /agent-send <agent> <message>", "warning");
        return;
      }
      try {
        await sendToAgent(recipient, text, ctx.cwd);
        ctx.ui.notify(`Message sent to ${recipient}`, "info");
      } catch (error) {
        ctx.ui.notify(`Failed to send message: ${String(error)}`, "error");
      }
    },
  });

  pi.registerCommand("agents", {
    description: "List live Pi agents for this project",
    getArgumentCompletions: (prefix) =>
      "all".startsWith(prefix)
        ? [{ value: "all", label: "all", description: "All projects" }]
        : null,
    handler: async (args, ctx) => {
      const scope = args.trim();
      if (scope && scope !== "all") {
        ctx.ui.notify("Usage: /agents [all]", "warning");
        return;
      }
      try {
        const command = ["--json", "session", "list"];
        if (scope === "all") {
          command.push("--all");
        }
        const output = await execPaj(command, ctx.cwd);
        const sessions = JSON.parse(output) as PajSession[];
        if (sessions.length === 0) {
          ctx.ui.notify("No live Pi agents found", "info");
          return;
        }
        const lines = sessions.map((session) => {
          const current = session.pid === process.pid ? "*" : " ";
          const branch = session.branch ?? "no branch";
          const task = session.task ? ` — ${session.task}` : "";
          return `${current} ${session.name} [${session.role}/${session.status}] ${branch}${task}`;
        });
        ctx.ui.notify(lines.join("\n"), "info");
      } catch (error) {
        ctx.ui.notify(`Failed to list agents: ${String(error)}`, "error");
      }
    },
  });

  pi.registerTool({
    name: "spawn_implementation_agent",
    label: "Spawn implementation agent",
    description:
      "Spawn an observable Pi implementation agent in an isolated Git worktree and branch",
    parameters: Type.Object({
      branch: Type.String({
        description: "Feature branch for the isolated worktree",
      }),
      prompt: Type.String({
        description: "Complete implementation task and acceptance criteria",
      }),
      name: Type.Optional(Type.String({ description: "Optional agent name" })),
      model: Type.Optional(Type.String({ description: "Optional Pi model" })),
      thinking: Type.Optional(
        Type.String({ description: "Optional thinking level" }),
      ),
    }),
    async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
      if (!activeSessionId) {
        throw new Error("paj session is not registered");
      }
      const args = [
        "agent",
        "spawn",
        "--branch",
        params.branch,
        "--prompt",
        params.prompt,
        "--parent",
        activeSessionId,
      ];
      if (params.name) {
        args.push("--name", params.name);
      }
      if (params.model) {
        args.push("--model", params.model);
      }
      if (params.thinking) {
        args.push("--thinking", params.thinking);
      }
      const result = await pi.exec("paj", args, { cwd: ctx.cwd });
      if (result.code !== 0) {
        throw new Error(
          result.stderr.trim() || `paj exited with code ${result.code}`,
        );
      }
      return {
        content: [{ type: "text", text: result.stdout.trim() }],
        details: { branch: params.branch, name: params.name },
      };
    },
  });

  pi.registerTool({
    name: "run_subagent",
    label: "Run subagent",
    description:
      "Run a clean foreground Pi subagent for bounded review, research, or diagnosis work",
    parameters: Type.Object({
      role: Type.String({
        description: "Specialist role such as review, research, or diagnosis",
      }),
      prompt: Type.String({ description: "Complete task for the subagent" }),
      artifact: Type.Optional(
        Type.String({
          description: "Repository-relative path for the final result",
        }),
      ),
      model: Type.Optional(Type.String({ description: "Optional Pi model" })),
      thinking: Type.Optional(
        Type.String({ description: "Optional thinking level" }),
      ),
    }),
    async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
      const args = [
        "agent",
        "run",
        "--role",
        params.role,
        "--prompt",
        params.prompt,
      ];
      if (params.artifact) {
        args.push("--artifact", params.artifact);
      }
      if (params.model) {
        args.push("--model", params.model);
      }
      if (params.thinking) {
        args.push("--thinking", params.thinking);
      }
      const result = await pi.exec("paj", args, { cwd: ctx.cwd });
      if (result.code !== 0) {
        throw new Error(
          result.stderr.trim() || `paj exited with code ${result.code}`,
        );
      }
      return {
        content: [{ type: "text", text: result.stdout.trim() }],
        details: { role: params.role, artifact: params.artifact },
      };
    },
  });

  pi.registerTool({
    name: "send_agent_message",
    label: "Send agent message",
    description: "Send a message to another live Pi agent listed by /agents",
    parameters: Type.Object({
      recipient: Type.String({ description: "Agent name or session ID" }),
      text: Type.String({ description: "Message to send" }),
    }),
    async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
      const message = await sendToAgent(params.recipient, params.text, ctx.cwd);
      return {
        content: [
          {
            type: "text",
            text: `Message ${message.id} sent to ${params.recipient}`,
          },
        ],
        details: message,
      };
    },
  });
}
