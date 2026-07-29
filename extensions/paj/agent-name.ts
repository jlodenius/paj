export interface PajSessionName {
  id: string;
  name: string;
}

export type PajExecutor = (args: string[], cwd: string) => Promise<string>;

export async function renamePajSession(
  execPaj: PajExecutor,
  sessionId: string,
  name: string,
  cwd: string,
): Promise<PajSessionName> {
  const output = await execPaj(
    ["--json", "session", "rename", sessionId, name],
    cwd,
  );
  return JSON.parse(output) as PajSessionName;
}
