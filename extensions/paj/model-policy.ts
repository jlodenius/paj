import { readFileSync } from "node:fs";
import { join } from "node:path";

export interface ModelCandidate {
  provider: string;
  id: string;
  name?: string;
}

export interface ModelPolicyRule {
  parentFamily: string;
  childFamily: string;
}

export interface ModelPolicy {
  rules: readonly ModelPolicyRule[];
  provider: "same";
  version: "latest";
  fallback: "inherit";
}

export interface SubagentModelResolution {
  model: string;
  source: "explicit" | "session" | "rule" | "inherit";
}

export interface ResolveSubagentModelOptions {
  parent?: ModelCandidate;
  available: readonly ModelCandidate[];
  policy: ModelPolicy;
  explicitModel?: string;
  sessionOverride?: string;
}

function objectAt(value: unknown, path: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(`${path} must be an object`);
  }
  return value as Record<string, unknown>;
}

function onlyKeys(
  value: Record<string, unknown>,
  keys: readonly string[],
  path: string,
): void {
  for (const key of Object.keys(value)) {
    if (!keys.includes(key)) {
      throw new Error(`Unknown key ${JSON.stringify(key)} at ${path}`);
    }
  }
}

function familyAt(value: unknown, path: string): string {
  if (
    typeof value !== "string" ||
    !/^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$/.test(value)
  ) {
    throw new Error(`${path} must be a lowercase model family name`);
  }
  return value;
}

function policyAt(value: unknown, path: string): ModelPolicy {
  const policy = objectAt(value, path);
  onlyKeys(policy, ["rules", "provider", "version", "fallback"], path);
  for (const [key, expected] of [
    ["provider", "same"],
    ["version", "latest"],
    ["fallback", "inherit"],
  ] as const) {
    if (Object.hasOwn(policy, key) && policy[key] !== expected) {
      throw new Error(`${path}.${key} must be ${JSON.stringify(expected)}`);
    }
  }

  const rawRules = Object.hasOwn(policy, "rules") ? policy.rules : [];
  if (!Array.isArray(rawRules)) {
    throw new Error(`${path}.rules must be an array`);
  }
  const parents = new Set<string>();
  const rules = rawRules.map((value, index) => {
    const rulePath = `${path}.rules[${index}]`;
    const rule = objectAt(value, rulePath);
    onlyKeys(rule, ["parentFamily", "childFamily"], rulePath);
    const parentFamily = familyAt(
      rule.parentFamily,
      `${rulePath}.parentFamily`,
    );
    const childFamily = familyAt(rule.childFamily, `${rulePath}.childFamily`);
    if (parents.has(parentFamily)) {
      throw new Error(
        `${rulePath}.parentFamily duplicates ${JSON.stringify(parentFamily)}`,
      );
    }
    parents.add(parentFamily);
    return { parentFamily, childFamily };
  });
  return { rules, provider: "same", version: "latest", fallback: "inherit" };
}

function assertUniqueJsonKeys(text: string): void {
  // JSON.parse discards duplicate keys; inspect its validated input before using the result.
  const tokens = text.match(/"(?:[^"\\]|\\.)*"|[{}\[\]:,]/g) ?? [];
  const stack: (Set<string> | null)[] = [];
  for (let index = 0; index < tokens.length; index++) {
    const token = tokens[index];
    if (token === "{") {
      stack.push(new Set());
    } else if (token === "[") {
      stack.push(null);
    } else if (token === "}" || token === "]") {
      stack.pop();
    } else if (token.startsWith('"') && tokens[index + 1] === ":") {
      const keys = stack.at(-1)!;
      const key: string = JSON.parse(token);
      if (keys!.has(key)) {
        throw new Error(`Duplicate JSON key ${JSON.stringify(key)}`);
      }
      keys!.add(key);
    }
  }
}

export function loadModelPolicy(agentDir: string): ModelPolicy {
  const path = join(agentDir, "paj.json");
  let text: string;
  try {
    text = readFileSync(path, "utf8");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") {
      return policyAt({}, "subagents.model");
    }
    throw new Error(
      `Cannot read Paj configuration ${path}: ${(error as Error).message}`,
      {
        cause: error,
      },
    );
  }

  try {
    const parsed: unknown = JSON.parse(text);
    assertUniqueJsonKeys(text);
    const config = objectAt(parsed, "paj.json");
    onlyKeys(config, ["subagents"], "paj.json");
    if (!Object.hasOwn(config, "subagents"))
      return policyAt({}, "subagents.model");
    const subagents = objectAt(config.subagents, "subagents");
    onlyKeys(subagents, ["model"], "subagents");
    return policyAt(
      Object.hasOwn(subagents, "model") ? subagents.model : {},
      "subagents.model",
    );
  } catch (error) {
    throw new Error(
      `Invalid Paj configuration ${path}: ${(error as Error).message}`,
      {
        cause: error,
      },
    );
  }
}

function qualified(model: ModelCandidate): string {
  return `${model.provider}/${model.id}`;
}

function exactMatches(
  value: string,
  available: readonly ModelCandidate[],
): ModelCandidate[] {
  const qualifiedMatches = available.filter(
    (model) => qualified(model) === value,
  );
  const matches = qualifiedMatches.length
    ? qualifiedMatches
    : available.filter((model) => model.id === value);
  return [
    ...new Map(matches.map((model) => [qualified(model), model])).values(),
  ];
}

function resolveChoice(
  choice: string,
  available: readonly ModelCandidate[],
  source: "explicit" | "session",
): SubagentModelResolution {
  if (typeof choice !== "string" || !choice.trim()) {
    throw new Error(`The ${source} model must be a nonempty exact model ID`);
  }
  let matches = exactMatches(choice, available);
  let suffix = "";
  if (!matches.length) {
    const thinking = /:([a-z][a-z0-9_-]*)$/.exec(choice);
    if (thinking) {
      suffix = thinking[0];
      matches = exactMatches(choice.slice(0, -suffix.length), available);
    }
  }
  if (matches.length > 1) {
    throw new Error(
      `The ${source} model ${JSON.stringify(choice)} is ambiguous; use one of: ` +
        matches.map(qualified).sort().join(", "),
    );
  }
  if (!matches.length) {
    throw new Error(
      `The ${source} model ${JSON.stringify(choice)} is not available; ` +
        "use an exact provider/id or a unique model ID",
    );
  }
  return { model: qualified(matches[0]) + suffix, source };
}

interface FamilyVersion {
  version: bigint[];
  snapshot?: string;
}

function validSnapshot(value: string): boolean {
  if (!/^\d{8}$/.test(value)) return false;
  const iso = `${value.slice(0, 4)}-${value.slice(4, 6)}-${value.slice(6, 8)}`;
  const date = new Date(`${iso}T00:00:00Z`);
  return (
    !Number.isNaN(date.getTime()) && date.toISOString().slice(0, 10) === iso
  );
}

function familyVersion(id: string, family: string): FamilyVersion | undefined {
  const gpt = new RegExp(
    `^gpt-(\\d+(?:\\.\\d+)*)-${family}(?:-(\\d{4}-\\d{2}-\\d{2}))?$`,
  ).exec(id);
  if (gpt) {
    const parts = gpt[1].split(".");
    const snapshot = gpt[2]?.replaceAll("-", "");
    if (parts.some((part) => !/^(0|[1-9]\d*)$/.test(part))) return undefined;
    if (snapshot && !validSnapshot(snapshot)) return undefined;
    return { version: parts.map((part) => BigInt(part)), snapshot };
  }

  const prefix = `claude-${family}-`;
  if (!id.startsWith(prefix)) return undefined;
  const parts = id.slice(prefix.length).split("-");
  let snapshot: string | undefined;
  if (/^\d{8}$/.test(parts.at(-1)!)) {
    snapshot = parts.pop();
    if (!validSnapshot(snapshot!)) return undefined;
  }
  if (!parts.length || parts.some((part) => !/^(0|[1-9]\d*)$/.test(part))) {
    return undefined;
  }
  return { version: parts.map((part) => BigInt(part)), snapshot };
}

function compareVersions(left: FamilyVersion, right: FamilyVersion): number {
  for (
    let index = 0;
    index < Math.max(left.version.length, right.version.length);
    index++
  ) {
    const a = left.version[index] ?? 0n;
    const b = right.version[index] ?? 0n;
    if (a !== b) return a > b ? 1 : -1;
  }
  if (!left.snapshot && right.snapshot) return 1;
  if (left.snapshot && !right.snapshot) return -1;
  if (left.snapshot !== right.snapshot)
    return left.snapshot! > right.snapshot! ? 1 : -1;
  return right.version.length - left.version.length;
}

export function resolveSubagentModel({
  parent,
  available,
  policy,
  explicitModel,
  sessionOverride,
}: ResolveSubagentModelOptions): SubagentModelResolution {
  const validatedPolicy = policyAt(policy, "policy");
  if (explicitModel !== undefined)
    return resolveChoice(explicitModel, available, "explicit");
  if (sessionOverride !== undefined)
    return resolveChoice(sessionOverride, available, "session");
  if (!parent) {
    throw new Error(
      "A parent model is required without an explicit model or session override",
    );
  }

  const matchingRules = validatedPolicy.rules.filter((rule) =>
    familyVersion(parent.id, rule.parentFamily),
  );
  if (matchingRules.length > 1) {
    throw new Error(
      `Parent model ${JSON.stringify(qualified(parent))} matches multiple family rules`,
    );
  }
  const rule = matchingRules[0];
  if (!rule) return { model: qualified(parent), source: "inherit" };

  const matches = available.flatMap((model) => {
    if (model.provider !== parent.provider) return [];
    const version = familyVersion(model.id, rule.childFamily);
    return version ? [{ model, version }] : [];
  });
  matches.sort((left, right) => {
    const versionOrder = compareVersions(right.version, left.version);
    if (versionOrder) return versionOrder;
    return left.model.id < right.model.id
      ? -1
      : left.model.id > right.model.id
        ? 1
        : 0;
  });
  if (!matches.length) {
    throw new Error(
      `No available canonical ${JSON.stringify(rule.childFamily)} model for provider ` +
        `${JSON.stringify(parent.provider)} required by rule ` +
        `${JSON.stringify(rule.parentFamily)} -> ${JSON.stringify(rule.childFamily)}`,
    );
  }
  return { model: qualified(matches[0].model), source: "rule" };
}
