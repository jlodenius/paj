import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

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
}

const COMMAND_TIMEOUT_MS = 5_000;
const HEARTBEAT_INTERVAL_MS = 10_000;

export default function pajExtension(pi: ExtensionAPI) {
  let activeSessionId: string | undefined;
  let heartbeatTimer: ReturnType<typeof setInterval> | undefined;
  let heartbeatPending: Promise<void> | undefined;

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
      "primary",
      "--cwd",
      ctx.cwd,
    ];
    const name = pi.getSessionName();
    if (name) {
      args.push("--name", name);
    }

    try {
      const output = await execPaj(args, ctx.cwd);
      const session = JSON.parse(output) as PajSession;
      activeSessionId = session.id;
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
    } catch (error) {
      if (ctx.hasUI) {
        ctx.ui.setStatus("paj", ctx.ui.theme.fg("error", "paj:disconnected"));
        ctx.ui.notify(`paj registration failed: ${String(error)}`, "error");
      }
    }
  });

  pi.on("session_shutdown", async (_event, ctx) => {
    if (heartbeatTimer) {
      clearInterval(heartbeatTimer);
      heartbeatTimer = undefined;
    }
    await heartbeatPending;
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
}
