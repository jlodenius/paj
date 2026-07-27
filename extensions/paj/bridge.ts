import { chmod, lstat, unlink } from "node:fs/promises";
import { createServer, type Server, type Socket } from "node:net";

const PROTOCOL_VERSION = 1;
const MAX_REQUEST_BYTES = 1024 * 1024;

interface BridgeRequest {
  version: number;
  id: string;
  method: string;
  params: {
    text: string;
  };
}

interface AssistantMessage {
  role: string;
  content?: Array<{ type?: string; text?: string }>;
  stopReason?: string;
  errorMessage?: string;
}

interface ActiveRequest {
  id: string;
  socket: Socket;
  completedMessages: string[];
  lastError?: string;
}

export interface BridgeActions {
  isIdle(): boolean;
  sendPrompt(text: string): void;
}

export class BridgeServer {
  private server: Server | undefined;
  private readonly sockets = new Set<Socket>();
  private active: ActiveRequest | undefined;
  private socketPath: string | undefined;

  constructor(private readonly actions: BridgeActions) {}

  async start(socketPath: string): Promise<void> {
    if (this.server) {
      throw new Error("bridge server is already running");
    }
    await removeStaleSocket(socketPath);
    const server = createServer((socket) => this.handleConnection(socket));
    await new Promise<void>((resolve, reject) => {
      const onError = (error: Error) => {
        server.off("listening", onListening);
        reject(error);
      };
      const onListening = () => {
        server.off("error", onError);
        resolve();
      };
      server.once("error", onError);
      server.once("listening", onListening);
      server.listen(socketPath);
    });
    try {
      await chmod(socketPath, 0o600);
    } catch (error) {
      server.close();
      await unlink(socketPath).catch(() => undefined);
      throw error;
    }
    this.server = server;
    this.socketPath = socketPath;
  }

  onMessageUpdate(event: {
    assistantMessageEvent?: { type?: string; delta?: string };
  }): void {
    const update = event.assistantMessageEvent;
    if (
      this.active &&
      update?.type === "text_delta" &&
      typeof update.delta === "string"
    ) {
      this.write(this.active.socket, {
        event: "delta",
        text: update.delta,
      });
    }
  }

  onMessageEnd(event: { message?: unknown }): void {
    if (!this.active || !isAssistantMessage(event.message)) {
      return;
    }
    const message = event.message;
    if (message.stopReason === "error" || message.stopReason === "aborted") {
      this.active.lastError =
        message.errorMessage ?? `generation ${message.stopReason}`;
      return;
    }
    this.active.lastError = undefined;
    const text = extractText(message);
    if (text) {
      this.active.completedMessages.push(text);
    }
  }

  onAgentSettled(): void {
    const active = this.active;
    if (!active) {
      return;
    }
    if (active.lastError) {
      this.fail(
        active.socket,
        active.id,
        "generation_failed",
        active.lastError,
      );
    } else {
      this.write(active.socket, {
        event: "complete",
        text: active.completedMessages.join("\n\n"),
      });
      active.socket.end();
    }
    this.active = undefined;
  }

  async stop(): Promise<void> {
    const active = this.active;
    this.active = undefined;
    if (active) {
      this.fail(
        active.socket,
        active.id,
        "shutting_down",
        "Pi session is shutting down",
      );
    }
    for (const socket of this.sockets) {
      if (socket !== active?.socket) {
        socket.destroy();
      }
    }
    this.sockets.clear();
    const server = this.server;
    this.server = undefined;
    if (server) {
      await new Promise<void>((resolve) => server.close(() => resolve()));
    }
    const socketPath = this.socketPath;
    this.socketPath = undefined;
    if (socketPath) {
      await unlink(socketPath).catch((error: NodeJS.ErrnoException) => {
        if (error.code !== "ENOENT") {
          throw error;
        }
      });
    }
  }

  private handleConnection(socket: Socket): void {
    this.sockets.add(socket);
    let buffer = Buffer.alloc(0);
    let handled = false;
    socket.on("data", (chunk: Buffer) => {
      if (handled) {
        return;
      }
      buffer = Buffer.concat([buffer, chunk]);
      if (buffer.length > MAX_REQUEST_BYTES) {
        handled = true;
        socket.destroy(new Error("bridge request exceeded maximum size"));
        return;
      }
      const newline = buffer.indexOf(0x0a);
      if (newline === -1) {
        return;
      }
      handled = true;
      const line = buffer.subarray(0, newline).toString("utf8");
      this.acceptRequest(socket, line);
    });
    socket.on("close", () => {
      this.sockets.delete(socket);
      if (this.active?.socket === socket) {
        this.active = undefined;
      }
    });
    socket.on("error", () => undefined);
  }

  private acceptRequest(socket: Socket, line: string): void {
    let request: BridgeRequest;
    try {
      request = JSON.parse(line) as BridgeRequest;
    } catch {
      socket.destroy(new Error("bridge request was not valid JSON"));
      return;
    }
    const validationError = validateRequest(request);
    if (validationError) {
      if (typeof request.id === "string") {
        this.fail(socket, request.id, "invalid_request", validationError);
      } else {
        socket.destroy(new Error(validationError));
      }
      return;
    }
    if (this.active || !this.actions.isIdle()) {
      this.fail(socket, request.id, "busy", "Pi session is busy");
      return;
    }
    this.active = {
      id: request.id,
      socket,
      completedMessages: [],
    };
    this.write(socket, { event: "accepted" });
    try {
      this.actions.sendPrompt(request.params.text);
    } catch (error) {
      this.fail(socket, request.id, "prompt_failed", String(error));
      this.active = undefined;
    }
  }

  private fail(
    socket: Socket,
    id: string,
    code: string,
    message: string,
  ): void {
    this.write(socket, { event: "error", id, code, message });
    socket.end();
  }

  private write(
    socket: Socket,
    event: Record<string, unknown> & { event: string },
  ): void {
    const id = event.id ?? this.active?.id;
    socket.write(
      `${JSON.stringify({ version: PROTOCOL_VERSION, id, ...event })}\n`,
    );
  }
}

async function removeStaleSocket(socketPath: string): Promise<void> {
  try {
    const metadata = await lstat(socketPath);
    if (!metadata.isSocket()) {
      throw new Error(`refusing to replace non-socket path ${socketPath}`);
    }
    await unlink(socketPath);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") {
      throw error;
    }
  }
}

function validateRequest(request: BridgeRequest): string | undefined {
  if (request.version !== PROTOCOL_VERSION) {
    return `unsupported bridge protocol version ${String(request.version)}`;
  }
  if (typeof request.id !== "string" || request.id.length === 0) {
    return "request id is required";
  }
  if (request.method !== "prompt") {
    return `unsupported bridge method ${String(request.method)}`;
  }
  if (
    typeof request.params !== "object" ||
    request.params === null ||
    typeof request.params.text !== "string" ||
    request.params.text.trim().length === 0
  ) {
    return "prompt text is required";
  }
  return undefined;
}

function isAssistantMessage(message: unknown): message is AssistantMessage {
  return (
    typeof message === "object" &&
    message !== null &&
    "role" in message &&
    message.role === "assistant"
  );
}

function extractText(message: AssistantMessage): string {
  return (message.content ?? [])
    .filter((content) => content.type === "text")
    .map((content) => content.text ?? "")
    .join("");
}
