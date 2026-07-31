export type PajSessionStatus = "idle" | "busy";

export type PajExecutor = (args: string[], cwd: string) => Promise<string>;

export async function setPajSessionStatus(
  execPaj: PajExecutor,
  sessionId: string,
  status: PajSessionStatus,
  cwd: string,
): Promise<void> {
  await execPaj(["session", "status", sessionId, status], cwd);
}
