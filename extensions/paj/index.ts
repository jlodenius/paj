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

export default function pajExtension(pi: ExtensionAPI) {
  let activeSessionId: string | undefined;
  let heartbeatTimer: ReturnType<typeof setInterval> | undefined;
  let heartbeatPending: Promise<void> | undefined;
  let messageTimer: ReturnType<typeof setInterval> | undefined;
  let messagePollPending: Promise<void> | undefined;
  let bridge: BridgeServer | undefined;

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

  const pollMessages = async (ctx: ExtensionContext) => {
    if (!activeSessionId) {
      return;
    }
    const output = await execPaj(
      ["--json", "message", "pending", activeSessionId],
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
      await execPaj(["message", "ack", activeSessionId, message.id], ctx.cwd);
    }
  };

  pi.on("session_start", async (_event, ctx) => {
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
    const name = pi.getSessionName() ?? process.env.PAJ_AGENT_NAME;
    if (name) {
      args.push("--name", name);
    }
    const task = process.env.PAJ_TASK;
    if (task) {
      args.push("--task", task);
    }

    try {
      const output = await execPaj(args, ctx.cwd);
      const session = JSON.parse(output) as PajSession;
      activeSessionId = session.id;
      if (!session.bridgeSocket) {
        throw new Error("paj session did not advertise a bridge socket");
      }
      bridge = new BridgeServer({
        isIdle: () => ctx.isIdle(),
        sendPrompt: (text) => pi.sendUserMessage(text),
      });
      await bridge.start(session.bridgeSocket);
      heartbeatTimer = setInterval(() => {
        if (!activeSessionId || heartbeatPending) {
          return;
        }
        heartbeatPending = execPaj(
          ["session", "heartbeat", activeSessionId],
          ctx.cwd,
        )
          .then(() => ctx.ui.setStatus("paj", undefined))
          .catch((error: unknown) => {
            if (ctx.hasUI) {
              ctx.ui.setStatus(
                "paj",
                ctx.ui.theme.fg("error", "paj:disconnected"),
              );
            }
            console.error("paj heartbeat failed", error);
          })
          .finally(() => {
            heartbeatPending = undefined;
          });
      }, HEARTBEAT_INTERVAL_MS);
      const poll = () => {
        if (messagePollPending) {
          return;
        }
        messagePollPending = pollMessages(ctx)
          .catch((error: unknown) =>
            console.error("paj message poll failed", error),
          )
          .finally(() => {
            messagePollPending = undefined;
          });
      };
      messageTimer = setInterval(poll, MESSAGE_POLL_INTERVAL_MS);
      poll();
    } catch (error) {
      const failedBridge = bridge;
      bridge = undefined;
      if (failedBridge) {
        await failedBridge.stop().catch(() => undefined);
      }
      const sessionId = activeSessionId;
      activeSessionId = undefined;
      if (sessionId) {
        await execPaj(["session", "unregister", sessionId], ctx.cwd).catch(
          () => undefined,
        );
      }
      if (ctx.hasUI) {
        ctx.ui.setStatus("paj", ctx.ui.theme.fg("error", "paj:disconnected"));
        ctx.ui.notify(`paj registration failed: ${String(error)}`, "error");
      }
    }
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
    if (heartbeatTimer) {
      clearInterval(heartbeatTimer);
      heartbeatTimer = undefined;
    }
    if (messageTimer) {
      clearInterval(messageTimer);
      messageTimer = undefined;
    }
    await Promise.all([heartbeatPending, messagePollPending]);
    const activeBridge = bridge;
    bridge = undefined;
    if (activeBridge) {
      try {
        await activeBridge.stop();
      } catch (error) {
        console.error("paj bridge shutdown failed", error);
      }
    }
    const sessionId = activeSessionId;
    activeSessionId = undefined;
    if (sessionId) {
      try {
        await execPaj(["session", "unregister", sessionId], ctx.cwd);
      } catch (error) {
        console.error("paj unregister failed", error);
      }
    }
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
