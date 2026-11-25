import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { build } from "esbuild";

const compiled = await build({
  entryPoints: ["src/simpleMode.ts"],
  bundle: true,
  write: false,
  format: "esm",
  platform: "node",
});

const mod = await import(`data:text/javascript;base64,${Buffer.from(compiled.outputFiles[0].text).toString("base64")}`);

const {
  SERVER_SUPPORT_PROVIDER_ID,
  ensureServerSupportProvider,
  rankServerModelId,
  selectServerSceneModels,
  resolveSimpleModeTarget,
  mergeSimpleModeServerScenes,
} = mod;

test("ensureServerSupportProvider adds server-support when absent and reuses existing preset", () => {
  const empty = ensureServerSupportProvider({ provider: { openai: { api: "openai-responses" } } });
  assert.equal(empty.providerId, SERVER_SUPPORT_PROVIDER_ID);
  assert.deepEqual(empty.config.provider["server-support"], { preset: "server-support" });
  assert.deepEqual(empty.config.provider.openai, { api: "openai-responses" });

  const existing = ensureServerSupportProvider({
    provider: { team: { preset: "server-support", name: "Team" } },
  });
  assert.equal(existing.providerId, "team");
  assert.equal(existing.provider.name, "Team");
  assert.equal(Object.keys(existing.config.provider).length, 1);
});

test("selectServerSceneModels prefers deepseek-v4.1-flash, resolves fetched variants when options are pending, and picks image-capable model for computer", () => {
  const fetched = {
    "gpt-5": {
      name: "GPT-5",
      modalities: { input: ["text", "image"] },
    },
    "deepseek/deepseek-v4.1-flash": {
      name: "DeepSeek V4.1 Flash",
      modalities: { input: ["text"] },
      variants: { none: {}, medium: {}, high: {} },
    },
  };
  const lyraOptions = {
    configOptions: [
      {
        id: "model",
        options: [
          { value: "server-support/gpt-5", name: "Team / GPT-5", _meta: { "codex.ai/supportsImages": true } },
          { value: "server-support/deepseek/deepseek-v4.1-flash/variant/none", name: "DeepSeek · None" },
          { value: "server-support/deepseek/deepseek-v4.1-flash/variant/medium", name: "DeepSeek · Medium" },
          { value: "server-support/deepseek/deepseek-v4.1-flash/variant/high", name: "DeepSeek · High" },
        ],
      },
    ],
    modes: null,
  };

  const selected = selectServerSceneModels("server-support", fetched, lyraOptions);
  assert.equal(selected.lightweight.agentKind, "lyra");
  assert.equal(selected.lightweight.model, "server-support/deepseek/deepseek-v4.1-flash/variant/medium");
  assert.equal(selected.coding.model, "server-support/deepseek/deepseek-v4.1-flash/variant/medium");
  assert.equal(selected.computer.model, "server-support/gpt-5");
  assert.equal(selected.displayName, "DeepSeek V4.1 Flash");

  // lyraOptions 仍在 pending（options 为空）时，也应能从 fetchedModels.variants 解析 variant
  const pendingSelected = selectServerSceneModels("server-support", fetched, {
    configOptions: [{ id: "model", options: [] }],
    modes: null,
  });
  assert.equal(pendingSelected.lightweight.model, "server-support/deepseek/deepseek-v4.1-flash/variant/medium");
  assert.equal(pendingSelected.computer.model, "server-support/gpt-5");

  // 服务端明确指定模型与分场景模型时，严格按服务端指定使用
  const specified = selectServerSceneModels(
    "server-support",
    {
      "deepseek-v4.1-flash": { name: "DeepSeek V4.1 Flash", modalities: { input: ["text", "image"] } },
      "opencode-go/glm-5.3": { name: "GLM-5.3", serverModel: true, serverModelVariant: "high" },
      "opendesign/gpt-5": { name: "GPT-5", serverComputer: true, modalities: { input: ["text", "image"] } },
    },
    null,
  );
  assert.equal(specified.lightweight.model, "server-support/opencode-go/glm-5.3/variant/high");
  assert.equal(specified.coding.model, "server-support/opencode-go/glm-5.3/variant/high");
  assert.equal(specified.computer.model, "server-support/opendesign/gpt-5");
});

test("selectServerSceneModels falls back to first server model and throws when empty", () => {
  const fallback = selectServerSceneModels(
    "team",
    { "claude-sonnet-4": { name: "Claude Sonnet 4" } },
    null,
  );
  assert.equal(fallback.lightweight.model, "team/claude-sonnet-4");
  assert.equal(fallback.coding.model, "team/claude-sonnet-4");
  assert.equal(fallback.computer.model, "team/claude-sonnet-4");

  assert.throws(() => selectServerSceneModels("server-support", {}, null), /服务端未返回可用模型/);
});

test("resolveSimpleModeTarget uses sceneModels first and falls back to server-support choices", () => {
  const choices = [
    { value: "openai/gpt-4.1", name: "GPT-4.1" },
    { value: "server-support/deepseek-v4.1-flash/variant/none", name: "Flash None" },
    { value: "server-support/deepseek-v4.1-flash/variant/medium", name: "Flash Medium" },
  ];
  const fromScene = resolveSimpleModeTarget(
    {
      sceneModels: {
        lightweight: [{ agentKind: "lyra", model: "server-support/deepseek-v4.1-flash/variant/medium" }],
        coding: [],
        computer: [],
      },
    },
    choices,
  );
  assert.deepEqual(fromScene, {
    agentKind: "lyra",
    model: "server-support/deepseek-v4.1-flash/variant/medium",
  });

  const fromFallback = resolveSimpleModeTarget(
    { sceneModels: { lightweight: [], coding: [], computer: [] } },
    choices,
  );
  assert.deepEqual(fromFallback, {
    agentKind: "lyra",
    model: "server-support/deepseek-v4.1-flash/variant/medium",
  });
  assert.equal(rankServerModelId("deepseek/deepseek-v4.1-flash"), 0);
});

test("OnboardingWizard does not call onCleanup inside onSettled", () => {
  const source = readFileSync(new URL("../src/components/OnboardingWizard.tsx", import.meta.url), "utf8");
  assert.doesNotMatch(source, /onCleanup\(/);
});

test("mergeSimpleModeServerScenes syncs server-specified models only in simpleMode and is idempotent", () => {
  const base = {
    simpleMode: true,
    sceneModels: {
      lightweight: [{ agentKind: "lyra", model: "server-support/old" }],
      coding: [{ agentKind: "lyra", model: "server-support/old" }],
      computer: [{ agentKind: "lyra", model: "server-support/old" }],
    },
  };
  assert.equal(
    mergeSimpleModeServerScenes({ ...base, simpleMode: false }, {
      lightweight: "server-support/opencode-go/deepseek-v4.1-flash",
      coding: "server-support/opencode-go/deepseek-v4.1-flash",
      computer: "server-support/opendesign/gpt-5",
    }),
    null,
  );
  const updated = mergeSimpleModeServerScenes(base, {
    lightweight: "server-support/opencode-go/deepseek-v4.1-flash/variant/medium",
    coding: "server-support/opencode-go/deepseek-v4.1-flash/variant/medium",
    computer: "server-support/opendesign/gpt-5",
  });
  assert.deepEqual(updated.sceneModels, {
    lightweight: [{ agentKind: "lyra", model: "server-support/opencode-go/deepseek-v4.1-flash/variant/medium" }],
    coding: [{ agentKind: "lyra", model: "server-support/opencode-go/deepseek-v4.1-flash/variant/medium" }],
    computer: [{ agentKind: "lyra", model: "server-support/opendesign/gpt-5" }],
  });
  // 已同步后再次调用应返回 null，避免重复写盘
  assert.equal(
    mergeSimpleModeServerScenes(updated, {
      lightweight: "server-support/opencode-go/deepseek-v4.1-flash/variant/medium",
      coding: "server-support/opencode-go/deepseek-v4.1-flash/variant/medium",
      computer: "server-support/opendesign/gpt-5",
    }),
    null,
  );
});
