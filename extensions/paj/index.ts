import type {
  ExtensionAPI,
  ExtensionContext,
} from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

import { renamePajSession } from "./agent-name.ts";
import {
  BridgeServer,
  type EditorRequest,
  type ProposalAction,
} from "./bridge.ts";
import {
  isToolAllowed,
  requestPolicy,
  requestPrompt,
} from "./editor-request.ts";
import { deliverPendingMessages } from "./message-delivery.ts";
import { registerProposalTool } from "./proposal-tool.ts";
import {
  type PajSessionStatus,
  setPajSessionStatus,
} from "./session-status.ts";
import {
  cleanupSubagents,
  combineSubagents,
  formatSubagents,
  shouldStopSubagents,
  type ListedSubagent,
  type SpawnRecord,
} from "./subagents.ts";

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
  parentPiSessionId?: string;
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
  let desiredStatus: PajSessionStatus = "idle";
  let statusUpdatePending: Promise<void> | undefined;
  let lastMessagePollError: string | undefined;
  const deliveredMessageIds = new Set<string>();
  let shuttingDown = false;
  const setProposalToolActive = registerProposalTool(pi, (actions) => {
    if (!bridge) {
      throw new Error("paj_propose_changes requires an active bridge request");
    }
    return bridge.submitProposals(actions);
  });

  const setToolActive = (name: string, active: boolean) => {
    const current = pi.getActiveTools();
    if (active && !current.includes(name)) {
      pi.setActiveTools([...current, name]);
    } else if (!active && current.includes(name)) {
      pi.setActiveTools(current.filter((candidate) => candidate !== name));
    }
  };

  pi.registerTool({
    name: "paj_editor_context",
    label: "Paj editor context",
    description:
      "Return the structured source selection for the active Paj editor request",
    parameters: Type.Object({}),
    async execute() {
      const request = bridge?.getActiveRequest();
      if (!request || !("source" in request)) {
        throw new Error("paj_editor_context requires an active source request");
      }
      return {
        content: [{ type: "text", text: JSON.stringify(request.source) }],
        details: { source: request.source },
      };
    },
  });

  const setRequestToolsActive = (request: EditorRequest | undefined) => {
    const readOnly = request !== undefined && request.kind !== "acceptAction";
    setProposalToolActive(readOnly);
    setToolActive(
      "paj_editor_context",
      request !== undefined && "source" in request,
    );
  };

  const sendEditorRequest = (
    request: EditorRequest,
    acceptedAction?: ProposalAction,
  ) => {
    pi.sendUserMessage(requestPrompt(request, acceptedAction));
  };

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

  const renameAgent = async (name: string, cwd: string) => {
    await registrationPending?.catch(() => undefined);
    const sessionId = activeSessionId;
    if (!sessionId) {
      throw new Error("paj session is not registered");
    }
    const session = await renamePajSession(execPaj, sessionId, name, cwd);
    if (sessionId !== activeSessionId) {
      throw new Error("paj registration changed while renaming");
    }
    registeredName = session.name;
    const spawnId = process.env.PAJ_SPAWN_ID;
    if (spawnId) {
      await execPaj(
        [
          "subagent",
          "bind",
          spawnId,
          "--child-pi-session-id",
          process.env.PAJ_PI_SESSION_ID ?? "",
          "--child-paj-session-id",
          session.id,
          "--name",
          session.name,
        ],
        cwd,
      );
    }
    return session.name;
  };

  const pollMessages = async (ctx: ExtensionContext, sessionId: string) => {
    const output = await execPaj(
      ["--json", "message", "pending", sessionId],
      ctx.cwd,
    );
    const messages = JSON.parse(output) as PajMessage[];
    await deliverPendingMessages(
      messages,
      deliveredMessageIds,
      (message, immediately) => {
        const content = `[Message from ${message.from.name}]\n${message.text}`;
        if (immediately) {
          pi.sendUserMessage(content);
        } else {
          pi.sendUserMessage(content, { deliverAs: "followUp" });
        }
      },
      (message) =>
        execPaj(["message", "ack", sessionId, message.id], ctx.cwd).then(
          () => undefined,
        ),
      ctx.isIdle(),
    );
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
    await statusUpdatePending?.catch(() => undefined);
    if (previousBridge) {
      await previousBridge
        .stop()
        .catch((error: unknown) =>
          console.error("paj bridge shutdown failed", error),
        );
    }
    setRequestToolsActive(undefined);
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
    const parentPiSessionId = process.env.PAJ_PARENT_PI_SESSION_ID;
    if (parentPiSessionId) {
      args.push("--parent-pi-session-id", parentPiSessionId);
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
      sendRequest: sendEditorRequest,
      cancelRequest: () => ctx.abort(),
      setRequestToolsActive,
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
    const spawnId = process.env.PAJ_SPAWN_ID;
    if (spawnId) {
      await execPaj(
        [
          "subagent",
          "bind",
          spawnId,
          "--child-pi-session-id",
          ctx.sessionManager.getSessionId(),
          "--child-paj-session-id",
          session.id,
          "--name",
          session.name,
        ],
        ctx.cwd,
      );
    }
    if (session.status !== desiredStatus) {
      await queueStatusUpdate(ctx, desiredStatus);
    }
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
    operation: "heartbeat" | "message poll" | "status update",
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

  const queueStatusUpdate = (
    ctx: ExtensionContext,
    status: PajSessionStatus,
  ): Promise<void> => {
    desiredStatus = status;
    const previous = statusUpdatePending?.catch(() => undefined);
    const update = (previous ?? Promise.resolve()).then(async () => {
      const sessionId = activeSessionId;
      if (!sessionId) {
        return;
      }
      try {
        await setPajSessionStatus(execPaj, sessionId, status, ctx.cwd);
      } catch (error) {
        handleOperationalFailure(ctx, "status update", sessionId, error);
        throw error;
      }
    });
    statusUpdatePending = update;
    void update.finally(() => {
      if (statusUpdatePending === update) {
        statusUpdatePending = undefined;
      }
    }).catch(() => undefined);
    return update;
  };

  const listAgents = async (all: boolean, cwd: string) => {
    const command = ["--json", "session", "list"];
    if (all) {
      command.push("--all");
    }
    return JSON.parse(await execPaj(command, cwd)) as PajSession[];
  };

  const listSubagents = async (ctx: ExtensionContext): Promise<ListedSubagent[]> => {
    const parentPiSessionId = ctx.sessionManager.getSessionId();
    const [recordsOutput, sessions] = await Promise.all([
      execPaj(
        ["--json", "subagent", "list", "--parent-pi-session-id", parentPiSessionId],
        ctx.cwd,
      ),
      listAgents(true, ctx.cwd),
    ]);
    return combineSubagents(JSON.parse(recordsOutput) as SpawnRecord[], sessions);
  };

  const stopSubagents = async (ctx: ExtensionContext) => {
    const children = await listSubagents(ctx);
    await cleanupSubagents(
      children,
      async (child) => {
        const result = await pi.exec(
          "tmux",
          ["-L", "paj", "kill-session", "-t", `=${child.tmuxName}`],
          { cwd: ctx.cwd, timeout: COMMAND_TIMEOUT_MS },
        );
        if (result.code !== 0) {
          throw new Error(
            result.stderr.trim() || `failed to stop tmux session ${child.tmuxName}`,
          );
        }
      },
      async (child) => {
        if (child.childPajSessionId) {
          await execPaj(
            ["session", "unregister", child.childPajSessionId],
            ctx.cwd,
          ).catch((error: unknown) => {
            if (!isMissingSessionError(error)) {
              throw error;
            }
          });
        }
        await execPaj(["subagent", "remove", child.spawnId], ctx.cwd);
      },
    );
  };

  pi.on("session_start", async (_event, ctx) => {
    shuttingDown = false;
    setRequestToolsActive(undefined);
    process.env.PAJ_PI_SESSION_ID = ctx.sessionManager.getSessionId();
    process.env.PAJ_PI_SESSION_PID = String(process.pid);
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

  pi.on("session_info_changed", async (event, ctx) => {
    if (!event.name || event.name === registeredName) {
      return;
    }
    if (!activeSessionId) {
      registeredName = event.name;
      return;
    }
    try {
      await renameAgent(event.name, ctx.cwd);
    } catch (error) {
      console.error("paj rename failed", error);
      if (ctx.hasUI) {
        ctx.ui.notify(`Failed to rename Paj agent: ${String(error)}`, "error");
      }
    }
  });

  pi.on("before_agent_start", (event) => {
    const request = bridge?.getActiveRequest();
    if (!request) {
      return;
    }
    return {
      systemPrompt: `${event.systemPrompt}\n\n${requestPolicy(request)}`,
    };
  });

  pi.on("tool_call", (event) => {
    const request = bridge?.getActiveRequest();
    if (!request || isToolAllowed(request, event.toolName)) {
      return;
    }
    return {
      block: true,
      reason: `${event.toolName} is unavailable during a read-only Paj request`,
    };
  });

  pi.on("agent_start", async (_event, ctx) => {
    await queueStatusUpdate(ctx, "busy").catch(() => undefined);
  });

  pi.on("message_update", async (event) => {
    bridge?.onMessageUpdate(event);
  });

  pi.on("message_end", async (event) => {
    bridge?.onMessageEnd(event);
  });

  pi.on("agent_settled", async (_event, ctx) => {
    const result = bridge?.onAgentSettled();
    if (result === "needsFinalization") {
      pi.sendMessage(
        {
          customType: "paj-finalize-response",
          content: [
            "Finalize the preceding visible Paj response now.",
            "Call paj_propose_changes exactly once with every concrete unimplemented repository change it recommends, or an empty array when there are none.",
            "Do not add visible text or call any other tool.",
          ].join(" "),
          display: false,
        },
        { deliverAs: "followUp", triggerTurn: true },
      );
      return;
    }
    await queueStatusUpdate(ctx, "idle").catch(() => undefined);
  });

  pi.on("session_shutdown", async (event, ctx) => {
    shuttingDown = true;
    if (shouldStopSubagents(event.reason)) {
      await stopSubagents(ctx).catch((error: unknown) =>
        console.error("paj subagent cleanup failed", error),
      );
    }
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

  const getAgentName = async () => {
    await registrationPending?.catch(() => undefined);
    if (!registeredName) {
      throw new Error("paj session is not registered");
    }
    return registeredName;
  };

  pi.registerCommand("agent-name", {
    description: "Show this Pi agent's Paj name",
    handler: async (_args, ctx) => {
      try {
        ctx.ui.notify(await getAgentName(), "info");
      } catch (error) {
        ctx.ui.notify(`Failed to get agent name: ${String(error)}`, "error");
      }
    },
  });

  pi.registerCommand("agent-rename", {
    description: "Rename this Pi agent in Paj",
    handler: async (args, ctx) => {
      const name = args.trim();
      if (!name) {
        ctx.ui.notify("Usage: /agent-rename <new-name>", "warning");
        return;
      }
      try {
        const renamed = await renameAgent(name, ctx.cwd);
        pi.setSessionName(renamed);
        ctx.ui.notify(`Paj agent renamed to ${renamed}`, "info");
      } catch (error) {
        ctx.ui.notify(`Failed to rename agent: ${String(error)}`, "error");
      }
    },
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
        const sessions = await listAgents(scope === "all", ctx.cwd);
        if (sessions.length === 0) {
          ctx.ui.notify("No live Pi agents found", "info");
          return;
        }
        const lines = sessions.map((session) => {
          const current = session.pid === process.pid ? "*" : " ";
          const branch = session.branch ?? "no branch";
          const task = session.task ? ` — ${session.task}` : "";
          const parent = session.parentPiSessionId
            ? ` parent:${session.parentPiSessionId}`
            : "";
          return `${current} ${session.name} [${session.role}/${session.status}]${parent} ${branch}${task}`;
        });
        ctx.ui.notify(lines.join("\n"), "info");
      } catch (error) {
        ctx.ui.notify(`Failed to list agents: ${String(error)}`, "error");
      }
    },
  });

  pi.registerCommand("subagents", {
    description: "List active tmux subagents owned by this Pi session",
    handler: async (_args, ctx) => {
      try {
        const children = await listSubagents(ctx);
        if (children.length === 0) {
          ctx.ui.notify("No active subagents found", "info");
          return;
        }
        const message =
          ctx.mode === "tui"
            ? formatSubagents(children, {
                name: (value) =>
                  ctx.ui.theme.fg("warning", ctx.ui.theme.bold(value)),
                status: (value) =>
                  ctx.ui.theme.fg(
                    value === "idle"
                      ? "success"
                      : value === "busy"
                        ? "warning"
                        : "dim",
                    `[${value}]`,
                  ),
                label: (value) => ctx.ui.theme.fg("warning", value),
                value: (value) => ctx.ui.theme.fg("text", value),
                command: (value) => ctx.ui.theme.fg("accent", value),
              })
            : formatSubagents(children);
        ctx.ui.notify(message, "info");
      } catch (error) {
        ctx.ui.notify(`Failed to list subagents: ${String(error)}`, "error");
      }
    },
  });

  pi.registerTool({
    name: "get_agent_name",
    label: "Get agent name",
    description: "Return this Pi agent's own Paj name",
    parameters: Type.Object({}),
    async execute() {
      const name = await getAgentName();
      return {
        content: [{ type: "text", text: name }],
        details: { name },
      };
    },
  });

  pi.registerTool({
    name: "list_agents",
    label: "List agents",
    description: "List live Paj agents and their idle or busy status",
    parameters: Type.Object({
      all: Type.Optional(
        Type.Boolean({ description: "List agents across all projects" }),
      ),
    }),
    async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
      const sessions = await listAgents(params.all ?? false, ctx.cwd);
      return {
        content: [{ type: "text", text: JSON.stringify(sessions, null, 2) }],
        details: { sessions },
      };
    },
  });

  pi.registerTool({
    name: "list_sub_agents",
    label: "List subagents",
    description: "List active tmux subagents owned by this Pi session with status and attach commands",
    parameters: Type.Object({}),
    async execute(_toolCallId, _params, _signal, _onUpdate, ctx) {
      const subagents = await listSubagents(ctx);
      return {
        content: [{ type: "text", text: JSON.stringify(subagents, null, 2) }],
        details: { subagents },
      };
    },
  });

  pi.registerTool({
    name: "send_agent_message",
    label: "Send agent message",
    description: "Send a message to another live Pi agent by name, Paj ID, or exact Pi session ID",
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
