import { randomUUID } from "node:crypto";
import { chmod, lstat, unlink } from "node:fs/promises";
import { createServer, type Server, type Socket } from "node:net";

const MAX_REQUEST_BYTES = 1024 * 1024;

export interface EditorSource {
  path: string;
  startLine: number;
  endLine: number;
  content: string;
}

export type EditorRequest =
  | { kind: "query"; query: string; source: EditorSource }
  | { kind: "explain"; focus?: string; source: EditorSource }
  | { kind: "review"; focus?: string; source: EditorSource }
  | { kind: "followup"; question: string }
  | { kind: "acceptAction"; actionId: string };

interface BridgeRequest {
  id: string;
  method: "request";
  params: EditorRequest;
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
  request: EditorRequest;
  completedMessages: string[];
  proposals?: ProposalAction[];
  lastError?: string;
}

export interface BridgeActions {
  isIdle(): boolean;
  sendRequest(request: EditorRequest, acceptedAction?: ProposalAction): void;
  cancelRequest(): void;
  setRequestToolsActive?(request: EditorRequest | undefined): void;
}

export class BridgeServer {
  private server: Server | undefined;
  private readonly sockets = new Set<Socket>();
  private readonly proposals = new Map<string, ProposalAction>();
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

  getActiveRequest(): EditorRequest | undefined {
    return this.active?.request;
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
      } while (ids.has(id) || this.proposals.has(id));
      ids.add(id);
      const action = { id, ...proposal };
      this.proposals.set(id, action);
      return action;
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
        this.actions.cancelRequest();
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
    let acceptedAction: ProposalAction | undefined;
    if (request.params.kind === "acceptAction") {
      acceptedAction = this.proposals.get(request.params.actionId);
      if (!acceptedAction) {
        this.fail(
          socket,
          request.id,
          "unknown_action",
          "proposed action was not found or was already accepted",
        );
        return;
      }
      this.proposals.delete(request.params.actionId);
    }
    this.active = {
      id: request.id,
      socket,
      request: request.params,
      completedMessages: [],
    };
    try {
      this.actions.setRequestToolsActive?.(request.params);
      this.write(socket, { event: "accepted" });
      this.actions.sendRequest(request.params, acceptedAction);
    } catch (error) {
      if (acceptedAction) {
        this.proposals.set(acceptedAction.id, acceptedAction);
      }
      this.fail(socket, request.id, "request_failed", String(error));
      this.finishRequest();
    }
  }

  private finishRequest(): void {
    if (!this.active) {
      return;
    }
    this.active = undefined;
    this.actions.setRequestToolsActive?.(undefined);
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
    socket.write(`${JSON.stringify({ id, ...event })}\n`);
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
  if (!id || !isUuid(id)) {
    return { ok: false, error: "request id must be a UUID" };
  }
  if (value.method !== "request") {
    return {
      ok: false,
      id,
      error: `unsupported bridge method ${String(value.method)}`,
    };
  }
  const params = validateEditorRequest(value.params);
  if (!params.ok) {
    return { ok: false, id, error: params.error };
  }
  return {
    ok: true,
    request: { id, method: "request", params: params.request },
  };
}

type EditorRequestValidation =
  | { ok: true; request: EditorRequest }
  | { ok: false; error: string };

function validateEditorRequest(value: unknown): EditorRequestValidation {
  if (!isRecord(value) || typeof value.kind !== "string") {
    return { ok: false, error: "editor request kind is required" };
  }
  if (value.kind === "query") {
    const source = validateSource(value.source);
    if (
      !source ||
      !hasOnlyKeys(value, ["kind", "query", "source"]) ||
      !isNonEmptyString(value.query)
    ) {
      return { ok: false, error: "query request is invalid" };
    }
    return { ok: true, request: { kind: "query", query: value.query, source } };
  }
  if (value.kind === "explain" || value.kind === "review") {
    const source = validateSource(value.source);
    if (
      !source ||
      !hasOnlyKeys(value, ["kind", "focus", "source"]) ||
      (value.focus !== undefined && typeof value.focus !== "string")
    ) {
      return { ok: false, error: `${value.kind} request is invalid` };
    }
    return {
      ok: true,
      request: {
        kind: value.kind,
        source,
        ...(value.focus === undefined ? {} : { focus: value.focus }),
      },
    };
  }
  if (value.kind === "followup") {
    if (
      !hasOnlyKeys(value, ["kind", "question"]) ||
      !isNonEmptyString(value.question)
    ) {
      return { ok: false, error: "followup request is invalid" };
    }
    return { ok: true, request: { kind: "followup", question: value.question } };
  }
  if (value.kind === "acceptAction") {
    if (
      !hasOnlyKeys(value, ["kind", "actionId"]) ||
      typeof value.actionId !== "string" ||
      !isUuid(value.actionId)
    ) {
      return { ok: false, error: "acceptAction request is invalid" };
    }
    return {
      ok: true,
      request: { kind: "acceptAction", actionId: value.actionId },
    };
  }
  return { ok: false, error: `unsupported editor request kind ${value.kind}` };
}

function validateSource(value: unknown): EditorSource | undefined {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, ["path", "startLine", "endLine", "content"]) ||
    !isNonEmptyString(value.path) ||
    !Number.isInteger(value.startLine) ||
    !Number.isInteger(value.endLine) ||
    (value.startLine as number) < 1 ||
    (value.endLine as number) < (value.startLine as number) ||
    typeof value.content !== "string"
  ) {
    return undefined;
  }
  return {
    path: value.path,
    startLine: value.startLine as number,
    endLine: value.endLine as number,
    content: value.content,
  };
}

function hasOnlyKeys(value: Record<string, unknown>, keys: string[]): boolean {
  const allowed = new Set(keys);
  return Object.keys(value).every((key) => allowed.has(key));
}

function isNonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.trim().length > 0;
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
