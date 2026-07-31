import { randomUUID } from "node:crypto";
import { chmod, lstat, unlink } from "node:fs/promises";
import { createServer, type Server, type Socket } from "node:net";

const PROTOCOL_VERSION = 1;
const MAX_REQUEST_BYTES = 1024 * 1024;

interface BridgeRequest {
  version: number;
  id: string;
  method: "prompt";
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

export interface ProposalAction {
  id: string;
  title: string;
  description: string;
}

export interface ProposalInput {
  title: string;
  description: string;
}

interface ActiveRequest {
  id: string;
  socket: Socket;
  completedMessages: string[];
  proposals?: ProposalAction[];
  lastError?: string;
}

export interface BridgeActions {
  isIdle(): boolean;
  sendPrompt(text: string): void;
  cancelPrompt(): void;
  setProposalToolActive?(active: boolean): void;
}

export class BridgeServer {
  private server: Server | undefined;
  private readonly sockets = new Set<Socket>();
  private active: ActiveRequest | undefined;
  private socketPath: string | undefined;
  private readonly actions: BridgeActions;

  constructor(actions: BridgeActions) {
    this.actions = actions;
  }

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

  submitProposals(value: unknown): ProposalAction[] {
    const active = this.active;
    if (!active) {
      throw new Error("paj_propose_changes requires an active bridge request");
    }
    if (active.proposals !== undefined) {
      throw new Error("paj_propose_changes may only be called once per bridge request");
    }
    const proposals = validateProposals(value);
    const ids = new Set<string>();
    active.proposals = proposals.map((proposal) => {
      let id: string;
      do {
        id = randomUUID();
      } while (ids.has(id));
      ids.add(id);
      return { id, ...proposal };
    });
    return active.proposals;
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
        actions: active.proposals ?? [],
      });
      active.socket.end();
    }
    this.finishRequest();
  }

  async stop(): Promise<void> {
    const active = this.active;
    this.finishRequest();
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
        this.finishRequest();
        this.actions.cancelPrompt();
      }
    });
    socket.on("error", () => undefined);
  }

  private acceptRequest(socket: Socket, line: string): void {
    let value: unknown;
    try {
      value = JSON.parse(line);
    } catch {
      socket.destroy(new Error("bridge request was not valid JSON"));
      return;
    }
    const validation = validateRequest(value);
    if (!validation.ok) {
      if (validation.id) {
        this.fail(socket, validation.id, "invalid_request", validation.error);
      } else {
        socket.destroy(new Error(validation.error));
      }
      return;
    }
    const request = validation.request;
    if (this.active || !this.actions.isIdle()) {
      this.fail(socket, request.id, "busy", "Pi session is busy");
      return;
    }
    this.active = {
      id: request.id,
      socket,
      completedMessages: [],
    };
    try {
      this.actions.setProposalToolActive?.(true);
      this.write(socket, { event: "accepted" });
      this.actions.sendPrompt(request.params.text);
    } catch (error) {
      this.fail(socket, request.id, "prompt_failed", String(error));
      this.finishRequest();
    }
  }

  private finishRequest(): void {
    if (!this.active) {
      return;
    }
    this.active = undefined;
    this.actions.setProposalToolActive?.(false);
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

type RequestValidation =
  | { ok: true; request: BridgeRequest }
  | { ok: false; id?: string; error: string };

function validateRequest(value: unknown): RequestValidation {
  if (!isRecord(value)) {
    return { ok: false, error: "request must be a JSON object" };
  }
  const id = typeof value.id === "string" ? value.id : undefined;
  if (value.version !== PROTOCOL_VERSION) {
    return {
      ok: false,
      id,
      error: `unsupported bridge protocol version ${String(value.version)}`,
    };
  }
  if (!id || !isUuid(id)) {
    return { ok: false, error: "request id must be a UUID" };
  }
  if (value.method !== "prompt") {
    return {
      ok: false,
      id,
      error: `unsupported bridge method ${String(value.method)}`,
    };
  }
  if (
    !isRecord(value.params) ||
    typeof value.params.text !== "string" ||
    value.params.text.trim().length === 0
  ) {
    return { ok: false, id, error: "prompt text is required" };
  }
  return {
    ok: true,
    request: {
      version: PROTOCOL_VERSION,
      id,
      method: "prompt",
      params: { text: value.params.text },
    },
  };
}

function validateProposals(value: unknown): ProposalInput[] {
  if (!Array.isArray(value)) {
    throw new Error("paj_propose_changes actions must be an array");
  }
  if (value.length === 0) {
    throw new Error("paj_propose_changes requires at least one action");
  }
  if (value.length > 20) {
    throw new Error("paj_propose_changes accepts at most 20 actions");
  }
  return value.map((item, index) => {
    if (
      !isRecord(item) ||
      typeof item.title !== "string" ||
      typeof item.description !== "string" ||
      Object.keys(item).some(
        (key) => key !== "title" && key !== "description",
      )
    ) {
      throw new Error(`paj_propose_changes action ${index + 1} is invalid`);
    }
    if (item.title.trim().length === 0) {
      throw new Error(`paj_propose_changes action ${index + 1} title is required`);
    }
    if (Buffer.byteLength(item.title, "utf8") > 200) {
      throw new Error(`paj_propose_changes action ${index + 1} title exceeds 200 bytes`);
    }
    if (item.description.trim().length === 0) {
      throw new Error(
        `paj_propose_changes action ${index + 1} description is required`,
      );
    }
    if (Buffer.byteLength(item.description, "utf8") > 4000) {
      throw new Error(
        `paj_propose_changes action ${index + 1} description exceeds 4000 bytes`,
      );
    }
    return { title: item.title, description: item.description };
  });
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isUuid(value: string): boolean {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(
    value,
  );
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
