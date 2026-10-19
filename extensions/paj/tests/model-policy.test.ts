import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";

import {
  loadModelPolicy,
  resolveSubagentModel,
  type ModelCandidate,
  type ModelPolicy,
  type ResolveSubagentModelOptions,
} from "../model-policy.ts";

const policy: ModelPolicy = {
  rules: [
    { parentFamily: "astra", childFamily: "sol" },
    { parentFamily: "fable", childFamily: "opus" },
  ],
  provider: "same",
  version: "latest",
  fallback: "inherit",
};
const defaultPolicy: ModelPolicy = { ...policy, rules: [] };
const astra: ModelCandidate = { provider: "openai", id: "gpt-6-astra" };
const fable: ModelCandidate = { provider: "anthropic", id: "claude-fable-5-1" };
const available: ModelCandidate[] = [
  astra,
  fable,
  { provider: "openai", id: "gpt-5.6-sol" },
  { provider: "openai", id: "gpt-6-sol" },
  { provider: "openai", id: "gpt-6.1-sol" },
  { provider: "anthropic", id: "claude-opus-4-6" },
  { provider: "anthropic", id: "claude-opus-5" },
  { provider: "anthropic", id: "claude-opus-5-5" },
];

function configDir(t: TestContext, text?: string): string {
  const dir = mkdtempSync(join(tmpdir(), "paj-model-policy-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  if (text !== undefined) writeFileSync(join(dir, "paj.json"), text);
  return dir;
}

function resolve(options: Partial<ResolveSubagentModelOptions> = {}) {
  return resolveSubagentModel({ parent: astra, available, policy, ...options });
}

function candidates(provider: string, ids: string[]): ModelCandidate[] {
  return ids.map((id) => ({ provider, id }));
}

test("a missing configuration defaults to inheritance with no personal routing rules", (t) => {
  const dir = configDir(t);
  assert.deepEqual(loadModelPolicy(dir), defaultPolicy);
  assert.deepEqual(
    loadModelPolicy(join(dir, "missing-agent-dir")),
    defaultPolicy,
  );
  assert.deepEqual(resolve({ policy: loadModelPolicy(dir) }), {
    model: "openai/gpt-6-astra",
    source: "inherit",
  });
  assert.notStrictEqual(loadModelPolicy(dir).rules, loadModelPolicy(dir).rules);
});

test("configuration is read from the supplied agent directory", (t) => {
  const dir = configDir(t, JSON.stringify({ subagents: { model: policy } }));
  assert.deepEqual(loadModelPolicy(dir), policy);
});

for (const config of [
  {},
  { subagents: {} },
  { subagents: { model: {} } },
  { subagents: { model: { provider: "same" } } },
]) {
  test(`optional configuration sections use defaults: ${JSON.stringify(config)}`, (t) => {
    assert.deepEqual(
      loadModelPolicy(configDir(t, JSON.stringify(config))),
      defaultPolicy,
    );
  });
}

test("omitted policy settings default around configured generic rules", (t) => {
  const rules = [{ parentFamily: "orbit", childFamily: "sage" }];
  const dir = configDir(t, JSON.stringify({ subagents: { model: { rules } } }));
  assert.deepEqual(loadModelPolicy(dir), { ...defaultPolicy, rules });
});

for (const [name, config, message] of [
  ["null root", null, /paj.json must be an object/],
  ["array root", [], /paj.json must be an object/],
  ["primitive root", true, /paj.json must be an object/],
  ["unknown root key", { subagent: {} }, /Unknown key "subagent" at paj.json/],
  ["null subagents", { subagents: null }, /subagents must be an object/],
  ["array subagents", { subagents: [] }, /subagents must be an object/],
  [
    "unknown subagent key",
    { subagents: { models: {} } },
    /Unknown key "models" at subagents/,
  ],
  [
    "null model",
    { subagents: { model: null } },
    /subagents.model must be an object/,
  ],
  [
    "array model",
    { subagents: { model: [] } },
    /subagents.model must be an object/,
  ],
] as const) {
  test(`configuration rejects ${name}`, (t) => {
    const dir = configDir(t, JSON.stringify(config));
    assert.throws(() => loadModelPolicy(dir), message);
    assert.throws(
      () => loadModelPolicy(dir),
      (error: Error) => {
        assert.ok(error.message.includes(join(dir, "paj.json")));
        return true;
      },
    );
  });
}

for (const [name, model, message] of [
  ["unknown policy key", { versions: "latest" }, /Unknown key "versions"/],
  ["other provider modes", { provider: "any" }, /provider must be "same"/],
  ["null provider", { provider: null }, /provider must be "same"/],
  ["other version modes", { version: "oldest" }, /version must be "latest"/],
  ["numeric version", { version: 6 }, /version must be "latest"/],
  ["other fallback modes", { fallback: "error" }, /fallback must be "inherit"/],
  ["null rules", { rules: null }, /rules must be an array/],
  ["object rules", { rules: {} }, /rules must be an array/],
  ["null rule", { rules: [null] }, /rules\[0\] must be an object/],
  ["array rule", { rules: [[]] }, /rules\[0\] must be an object/],
  ["missing families", { rules: [{}] }, /parentFamily must be/],
  [
    "missing child",
    { rules: [{ parentFamily: "astra" }] },
    /childFamily must be/,
  ],
  [
    "empty family",
    { rules: [{ parentFamily: "", childFamily: "sol" }] },
    /parentFamily must be/,
  ],
  [
    "nonstring family",
    { rules: [{ parentFamily: 6, childFamily: "sol" }] },
    /parentFamily must be/,
  ],
  [
    "uppercase family",
    { rules: [{ parentFamily: "Astra", childFamily: "sol" }] },
    /parentFamily must be/,
  ],
  [
    "pattern family",
    { rules: [{ parentFamily: "astra.*", childFamily: "sol" }] },
    /parentFamily must be/,
  ],
  [
    "invalid child",
    { rules: [{ parentFamily: "astra", childFamily: "sol:high" }] },
    /childFamily must be/,
  ],
  [
    "unknown rule key",
    { rules: [{ parentFamily: "astra", childFamily: "sol", provider: "any" }] },
    /Unknown key "provider"/,
  ],
  [
    "duplicate parent",
    {
      rules: [policy.rules[0], { parentFamily: "astra", childFamily: "opus" }],
    },
    /parentFamily duplicates "astra"/,
  ],
  [
    "duplicate identical rule",
    { rules: [policy.rules[0], policy.rules[0]] },
    /parentFamily duplicates "astra"/,
  ],
] as const) {
  test(`configuration rejects ${name}`, (t) => {
    const dir = configDir(t, JSON.stringify({ subagents: { model } }));
    assert.throws(() => loadModelPolicy(dir), message);
  });
}

test("multiple parent families may route to the same child family", (t) => {
  const rules = [
    { parentFamily: "astra", childFamily: "sol" },
    { parentFamily: "fable", childFamily: "sol" },
  ];
  const dir = configDir(t, JSON.stringify({ subagents: { model: { rules } } }));
  assert.deepEqual(loadModelPolicy(dir).rules, rules);
});

for (const text of ["{", "", '{"subagents":{},}', "// comment\n{}"]) {
  test(`malformed JSON fails clearly: ${JSON.stringify(text)}`, (t) => {
    const dir = configDir(t, text);
    assert.throws(
      () => loadModelPolicy(dir),
      /Invalid Paj configuration .*paj\.json:/,
    );
  });
}

for (const text of [
  '{"subagents":{},"subagents":{}}',
  '{"subagents":{"model":{"provider":"same","provider":"same"}}}',
  '{"subagents":{"model":{"rules":[{"parentFamily":"astra","parentFamily":"fable","childFamily":"sol"}]}}}',
  '{"subagents":{},"sub\\u0061gents":{}}',
]) {
  test(`duplicate JSON keys fail rather than silently taking the last value: ${text}`, (t) => {
    assert.throws(
      () => loadModelPolicy(configDir(t, text)),
      /Duplicate JSON key/,
    );
  });
}

test("JSON key validation does not mistake punctuation within string values for keys", (t) => {
  const text = JSON.stringify({
    subagents: { model: { fallback: '{"x":1,"x":2}' } },
  });
  assert.throws(
    () => loadModelPolicy(configDir(t, text)),
    /fallback must be "inherit"/,
  );
});

test("read errors other than a missing file are not treated as default configuration", (t) => {
  const dir = configDir(t);
  mkdirSync(join(dir, "paj.json"));
  assert.throws(
    () => loadModelPolicy(dir),
    /Cannot read Paj configuration .*paj\.json:/,
  );
});

test("explicit selection takes precedence over session and routing", () => {
  assert.deepEqual(
    resolve({
      explicitModel: "anthropic/claude-opus-4-6:high",
      sessionOverride: "gpt-6-sol",
    }),
    {
      model: "anthropic/claude-opus-4-6:high",
      source: "explicit",
    },
  );
});

test("a session override takes precedence over routing", () => {
  assert.deepEqual(resolve({ sessionOverride: "gpt-5.6-sol:low" }), {
    model: "openai/gpt-5.6-sol:low",
    source: "session",
  });
});

test("routing selects the latest same-provider model for the configured family", () => {
  assert.deepEqual(resolve(), { model: "openai/gpt-6.1-sol", source: "rule" });
  assert.deepEqual(resolve({ parent: fable }), {
    model: "anthropic/claude-opus-5-5",
    source: "rule",
  });
});

test("unmatched parents inherit their exact provider/id even outside the available list", () => {
  const parent = { provider: "custom", id: "a/model:literal", name: "Astra" };
  assert.deepEqual(resolve({ parent, available: [] }), {
    model: "custom/a/model:literal",
    source: "inherit",
  });
});

test("an empty policy inherits instead of applying personal mappings", () => {
  assert.deepEqual(resolve({ policy: defaultPolicy }), {
    model: "openai/gpt-6-astra",
    source: "inherit",
  });
});

test("family names and routing pairs are generic rather than personal defaults", () => {
  assert.deepEqual(
    resolve({
      parent: { provider: "gateway", id: "claude-orbit-2" },
      available: [{ provider: "gateway", id: "gpt-8-sage" }],
      policy: {
        ...defaultPolicy,
        rules: [{ parentFamily: "orbit", childFamily: "sage" }],
      },
    }),
    { model: "gateway/gpt-8-sage", source: "rule" },
  );
  assert.deepEqual(
    resolve({
      parent: { provider: "gateway", id: "gpt-6-astra" },
      available: [{ provider: "gateway", id: "claude-opus-5" }],
      policy: {
        ...defaultPolicy,
        rules: [{ parentFamily: "astra", childFamily: "opus" }],
      },
    }),
    { model: "gateway/claude-opus-5", source: "rule" },
  );
});

for (const source of ["explicitModel", "sessionOverride"] as const) {
  test(`${source} accepts qualified IDs and unique exact IDs with thinking suffixes`, () => {
    for (const choice of ["gpt-6-sol", "openai/gpt-6-sol"]) {
      for (const suffix of [
        "",
        ":off",
        ":minimal",
        ":low",
        ":medium",
        ":high",
        ":xhigh",
      ]) {
        assert.deepEqual(resolve({ [source]: choice + suffix }), {
          model: "openai/gpt-6-sol" + suffix,
          source: source === "explicitModel" ? "explicit" : "session",
        });
      }
    }
  });

  test(`${source} needs no parent model`, () => {
    assert.equal(
      resolve({ parent: undefined, [source]: "gpt-6-sol" }).model,
      "openai/gpt-6-sol",
    );
  });

  test(`${source} rejects ambiguous IDs even when one provider matches the parent`, () => {
    const models = [...available, { provider: "azure", id: "gpt-6-sol" }];
    assert.throws(
      () => resolve({ available: models, [source]: "gpt-6-sol" }),
      /ambiguous.*azure\/gpt-6-sol.*openai\/gpt-6-sol/,
    );
    assert.throws(
      () => resolve({ available: models, [source]: "gpt-6-sol:high" }),
      /ambiguous/,
    );
    assert.equal(
      resolve({ available: models, [source]: "azure/gpt-6-sol:high" }).model,
      "azure/gpt-6-sol:high",
    );
  });

  test(`${source} rejects arbitrary patterns, display names, and unavailable IDs`, () => {
    for (const choice of [
      "",
      " ",
      "sol",
      "gpt-6",
      "GPT-6-SOL",
      "openai/*sol",
      "unknown/gpt-6-sol",
      "gpt-7-sol",
      "gpt-6-sol:",
      " gpt-6-sol",
    ]) {
      assert.throws(
        () => resolve({ [source]: choice }),
        /nonempty exact model ID|not available/,
      );
    }
    assert.throws(
      () =>
        resolve({
          available: [
            { provider: "openai", id: "gpt-6-sol", name: "Best Sol" },
          ],
          [source]: "Best Sol",
        }),
      /not available/,
    );
  });

  test(`${source} may intentionally choose an available preview, alias, or noncanonical ID`, () => {
    for (const id of [
      "gpt-7-sol-preview",
      "sol-latest",
      "arbitrary/model-id",
    ]) {
      assert.equal(
        resolve({
          available: [{ provider: "custom", id }],
          [source]: id + ":high",
        }).model,
        `custom/${id}:high`,
      );
    }
  });
}

test("an invalid explicit choice does not fall through to session, rule, or inheritance", () => {
  assert.throws(
    () =>
      resolve({ explicitModel: "not-available", sessionOverride: "gpt-6-sol" }),
    /explicit model .*not available/,
  );
});

test("an invalid session override does not fall through to routing or inheritance", () => {
  assert.throws(
    () => resolve({ sessionOverride: "not-available" }),
    /session model .*not available/,
  );
});

test("a valid explicit choice ignores an invalid session override", () => {
  assert.deepEqual(
    resolve({ explicitModel: "gpt-6-sol", sessionOverride: "not-available" }),
    {
      model: "openai/gpt-6-sol",
      source: "explicit",
    },
  );
});

test("inheriting the explicitly chosen parent still reports the selection source", () => {
  assert.deepEqual(resolve({ explicitModel: "gpt-6-astra" }), {
    model: "openai/gpt-6-astra",
    source: "explicit",
  });
});

test("no parent model fails clearly when inheritance or routing is needed", () => {
  assert.throws(
    () => resolve({ parent: undefined }),
    /parent model is required/,
  );
  assert.throws(
    () => resolve({ parent: undefined, policy: defaultPolicy }),
    /parent model is required/,
  );
});

test("nested IDs preserve the exact provider and ID", () => {
  const model = { provider: "openrouter", id: "anthropic/claude-opus-5-5" };
  for (const choice of [model.id, `openrouter/${model.id}`]) {
    assert.equal(
      resolve({ available: [model], explicitModel: choice + ":high" }).model,
      "openrouter/anthropic/claude-opus-5-5:high",
    );
  }
});

test("colon-bearing exact IDs are looked up before treating a suffix as thinking", () => {
  const models = candidates("custom", ["model", "model:high"]);
  assert.equal(
    resolve({ available: models, explicitModel: "model:high" }).model,
    "custom/model:high",
  );
  assert.equal(
    resolve({ available: models, explicitModel: "custom/model:high:low" })
      .model,
    "custom/model:high:low",
  );
});

test("duplicate registry entries for the same provider/id do not create ambiguity", () => {
  const model = { provider: "openai", id: "gpt-6-sol" };
  assert.equal(
    resolve({ available: [model, { ...model }], explicitModel: model.id })
      .model,
    "openai/gpt-6-sol",
  );
});

test("routing never crosses providers even when another provider has a newer model", () => {
  assert.equal(
    resolve({
      available: [
        { provider: "azure", id: "gpt-99-sol" },
        { provider: "openai", id: "gpt-6-sol" },
      ],
    }).model,
    "openai/gpt-6-sol",
  );
});

test("matched but unavailable target family throws rather than inheriting or crossing providers", () => {
  for (const models of [
    [],
    [astra],
    [{ provider: "azure", id: "gpt-6-sol" }],
  ]) {
    assert.throws(
      () => resolve({ available: models }),
      /No available canonical "sol" model for provider "openai".*"astra" -> "sol"/,
    );
  }
});

test("SOL versions are compared numerically, including minor versions and major transitions", () => {
  const ids = [
    "gpt-5.6-sol",
    "gpt-6-sol",
    "gpt-6.1-sol",
    "gpt-6.9-sol",
    "gpt-6.10-sol",
    "gpt-10-sol",
  ];
  for (let end = 1; end <= ids.length; end++) {
    assert.equal(
      resolve({ available: candidates("openai", ids.slice(0, end).reverse()) })
        .model,
      `openai/${ids[end - 1]}`,
    );
  }
});

test("Opus versions are compared numerically without mistaking snapshots for version components", () => {
  const ids = [
    "claude-opus-4-6-20261231",
    "claude-opus-5",
    "claude-opus-5-5",
    "claude-opus-5-9",
    "claude-opus-5-10",
    "claude-opus-10",
  ];
  for (let end = 1; end <= ids.length; end++) {
    assert.equal(
      resolve({
        parent: fable,
        available: candidates("anthropic", ids.slice(0, end).reverse()),
      }).model,
      `anthropic/${ids[end - 1]}`,
    );
  }
});

test("an undated Opus ID is preferred over dated snapshots of the same version", () => {
  const ids = [
    "claude-opus-5-5-20260101",
    "claude-opus-5-5",
    "claude-opus-5-5-20261231",
  ];
  for (const ordered of [ids, [...ids].reverse()]) {
    assert.equal(
      resolve({ parent: fable, available: candidates("anthropic", ordered) })
        .model,
      "anthropic/claude-opus-5-5",
    );
  }
});

test("a newer version snapshot wins over an older undated version", () => {
  assert.equal(
    resolve({
      parent: fable,
      available: candidates("anthropic", [
        "claude-opus-5",
        "claude-opus-5-5-20260101",
      ]),
    }).model,
    "anthropic/claude-opus-5-5-20260101",
  );
});

test("the newest dated snapshot is selected when no undated ID exists at that version", () => {
  assert.equal(
    resolve({
      parent: fable,
      available: candidates("anthropic", [
        "claude-opus-5-5-20260201",
        "claude-opus-5-5-20260101",
      ]),
    }).model,
    "anthropic/claude-opus-5-5-20260201",
  );
});

test("canonical GPT dated snapshots also prefer undated versions and otherwise sort by date", () => {
  const ids = ["gpt-6-sol-2026-02-01", "gpt-6-sol-2026-01-01"];
  assert.equal(
    resolve({ available: candidates("openai", ids) }).model,
    "openai/gpt-6-sol-2026-02-01",
  );
  assert.equal(
    resolve({ available: candidates("openai", [...ids, "gpt-6-sol"]) }).model,
    "openai/gpt-6-sol",
  );
});

test("aliases, previews, misleading names, and noncanonical IDs cannot outrank stable SOL", () => {
  const ids = [
    "gpt-6-sol",
    "gpt-99-sol-preview",
    "gpt-99-sol-latest",
    "gpt-99-sol-beta",
    "gpt-99-sol-mini",
    "gpt-06-sol",
    "gpt-6.01-sol",
    "gpt-99-sol-2026-02-30",
    "sol-latest",
    "unrelated-sol-99",
  ];
  const models = candidates("openai", ids);
  models.push({ provider: "openai", id: "unknown", name: "GPT-999 SOL" });
  assert.equal(resolve({ available: models }).model, "openai/gpt-6-sol");
});

test("aliases, previews, and invalid dates cannot outrank canonical Opus", () => {
  const ids = [
    "claude-opus-5",
    "claude-opus-latest",
    "claude-opus-99-preview",
    "claude-opus-99-1-latest",
    "claude-opus-05",
    "claude-opus-20260101",
    "claude-opus-99-20260230",
    "claude-opus-99-20261301",
    "claude-opus-99-20260132",
  ];
  assert.equal(
    resolve({ parent: fable, available: candidates("anthropic", ids) }).model,
    "anthropic/claude-opus-5",
  );
});

test("a rule with only preview or alias targets fails instead of silently inheriting", () => {
  assert.throws(
    () =>
      resolve({
        available: candidates("openai", ["gpt-99-sol-preview", "sol-latest"]),
      }),
    /No available canonical/,
  );
});

test("all specified Fable and Astra parent versions are recognized", () => {
  for (const id of [
    "claude-fable-5",
    "claude-fable-5-1",
    "claude-fable-5-1-20260101",
  ]) {
    assert.equal(
      resolve({ parent: { provider: "anthropic", id } }).model,
      "anthropic/claude-opus-5-5",
    );
  }
  assert.equal(resolve({ parent: astra }).model, "openai/gpt-6.1-sol");
});

test("Fable and Astra are also valid configurable target families", () => {
  for (const [family, ids, expected] of [
    [
      "fable",
      ["claude-fable-5", "claude-fable-5-1", "claude-fable-5-10"],
      "claude-fable-5-10",
    ],
    ["astra", ["gpt-5.9-astra", "gpt-6-astra"], "gpt-6-astra"],
  ] as const) {
    assert.equal(
      resolve({
        parent: { provider: "gateway", id: "gpt-6-sol" },
        available: candidates("gateway", [...ids]),
        policy: {
          ...defaultPolicy,
          rules: [{ parentFamily: "sol", childFamily: family }],
        },
      }).model,
      `gateway/${expected}`,
    );
  }
});

test("noncanonical parent IDs and display names do not accidentally trigger family rules", () => {
  for (const id of [
    "astra",
    "gpt-6-astra-preview",
    "gpt-6-astra-latest",
    "unrelated",
  ]) {
    const parent = { provider: "openai", id, name: "GPT-6 Astra" };
    assert.deepEqual(resolve({ parent }), {
      model: `openai/${id}`,
      source: "inherit",
    });
  }
});

test("same-family rules intentionally select latest rather than inheriting an older parent", () => {
  assert.deepEqual(
    resolve({
      parent: { provider: "openai", id: "gpt-5.6-sol" },
      policy: {
        ...defaultPolicy,
        rules: [{ parentFamily: "sol", childFamily: "sol" }],
      },
    }),
    { model: "openai/gpt-6.1-sol", source: "rule" },
  );
});

test("equivalent numeric versions have a deterministic preference for the shorter canonical version", () => {
  for (const ids of [
    ["gpt-6.0-sol", "gpt-6-sol"],
    ["gpt-6-sol", "gpt-6.0-sol"],
  ]) {
    assert.equal(
      resolve({ available: candidates("openai", ids) }).model,
      "openai/gpt-6-sol",
    );
  }
});

test("resolution does not mutate policy, parent, or available model order", () => {
  const input = { parent: astra, available, policy };
  const before = structuredClone(input);
  resolveSubagentModel(input);
  assert.deepEqual(input, before);
});

test("ambiguous family names cannot make routing depend on rule order", () => {
  const rules = [
    { parentFamily: "orbit", childFamily: "sol" },
    { parentFamily: "orbit-2", childFamily: "opus" },
  ];
  for (const ordered of [rules, [...rules].reverse()]) {
    assert.throws(
      () =>
        resolve({
          parent: { provider: "gateway", id: "claude-orbit-2-3" },
          policy: { ...defaultPolicy, rules: ordered },
        }),
      /matches multiple family rules/,
    );
  }
});

test("invalid policies are also rejected at the public resolver boundary", () => {
  const invalid = JSON.parse(
    '{"rules":[],"provider":"any","version":"latest","fallback":"inherit"}',
  );
  assert.throws(
    () => resolve({ policy: invalid }),
    /policy.provider must be "same"/,
  );
});
