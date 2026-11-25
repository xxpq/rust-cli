#!/usr/bin/env node
// rust-cli 平台层 option map 补丁的 Node 对拍夹具（docs/specs/rust-cli/model-options.md §12.1、platform.md §7）：
// 用 Node 的 Provider registry 解析内置配置，对每个可执行模型的每一档，用 Node 的编译器求 `reasoningLevel` 与
// `maxOutputTokens` 两个补丁，写成 `JSON.stringify` 的原文。在装好依赖的检出里运行：
//
//   node_modules/.bin/tsx scripts/rust-cli/provider-patches-golden.mts \
//     packages/rust-cli/crates/platform/assets/zcode-builtin.json \
//     packages/rust-cli/crates/platform/tests/fixtures/node-patches.json
//
// 输入与 rust 一侧（`catalog_tests.rs` 的 `patch_fixture`）相同：个人配置里每个内置模板一个 provider
// （`personal:<templateId>`，套这个模板，假的 API key）；账号层授权全部账号 provider。这样内置配置的每个 provider、
// 每个模板、每个模型都可执行。
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { compileModelOptionMap } from "../../packages/model-option-map/src/index.ts";
import {
  MutableAccountProviderConfigSource,
  ProviderConfig,
  ProviderConfigMap,
  ZhipuAccountAccessConfig,
  createAccountProviderConfigSnapshot,
} from "../../packages/provider/src/index.ts";
import { createNodeProviderRegistryRuntime } from "../../packages/provider-node/src/index.ts";

/** 个人 provider 的 ID 前缀与假 key（两边相同）。 */
const PERSONAL_PREFIX = "personal:";
const FIXTURE_KEY = "sk-patches-fixture";
const API_KEY = "api-key";
const PERSONAL_GROUP = "standard-personal";

const [builtinPath, out] = process.argv.slice(2);
const builtin = JSON.parse(readFileSync(builtinPath, "utf8"));
type Template = {
  templateId: string;
  config: { access?: { type?: string }; builtinModelIds?: string[] };
};
const templates: Template[] = builtin.config.providerConfigRules.templateRules;
const personal = {
  schemaVersion: 1,
  config: {
    providerConfigRules: {
      providerRules: templates.map((template) => ({
        providerId: `${PERSONAL_PREFIX}${template.templateId}`,
        templateId: template.templateId,
        enabled: true,
        // access 的 type 是判别字段：沿用模板的（没有时是普通 API key），只补 key。个人 provider 要写分组。
        config: {
          group: PERSONAL_GROUP,
          access: { type: template.config.access?.type ?? API_KEY, apiKey: FIXTURE_KEY },
        },
      })),
    },
    // 模板里缺省关着的模型也打开（用户能在设置里打开它们），补丁覆盖模板的每个模型。
    modelConfigRules: {
      providerModelRules: templates.flatMap((template) =>
        (template.config.builtinModelIds ?? []).map((modelId) => ({
          providerId: `${PERSONAL_PREFIX}${template.templateId}`,
          modelId,
          config: { enabled: true },
        })),
      ),
      manualProviderModelRules: [],
    },
  },
};
const scratch = mkdtempSync(join(tmpdir(), "provider-patches-golden-"));
const personalPath = join(scratch, "provider_config.json");
writeFileSync(personalPath, JSON.stringify(personal));

const accountSource = new MutableAccountProviderConfigSource();
const runtime = createNodeProviderRegistryRuntime({
  zcodeBuiltinFilePath: builtinPath,
  personalFilePath: personalPath,
  personalPollingIntervalMs: false,
  accountSource,
  // 个人配置无效时 Node 改名恢复、当作空配置；夹具要的是全部模板，无效就失败。
  onPersonalConfigRecovery: ({ error }) => {
    throw error;
  },
});
await runtime.configService.start?.();
const config = await runtime.configService.read();
accountSource.replace(
  createAccountProviderConfigSnapshot(
    config.zcodeBuiltinRevision,
    new ProviderConfigMap(
      config.zcodeBuiltinProviders
        .entries()
        .filter(([, provider]) => provider.access?.type === "zhipu-account")
        .map(([id]) => [
          id,
          new ProviderConfig({ access: new ZhipuAccountAccessConfig({ entitled: true }) }),
        ]),
    ),
  ),
  "fixture",
);
await runtime.start();
const models = runtime.registryService.listProviders().flatMap((provider) =>
  provider.models.map((model) => {
    const { reasoningLevel, maxOutputTokens } = model.config.optionSpecs;
    const reasoning = compileModelOptionMap(reasoningLevel.map, "reasoningLevel");
    const max = compileModelOptionMap(maxOutputTokens.map, "maxOutputTokens");
    return {
      provider: provider.providerId,
      model: model.modelId,
      apiType: provider.config.api.type,
      maxOutputTokens: JSON.stringify(max.evaluate(maxOutputTokens.max)),
      reasoningLevel: Object.fromEntries(
        reasoningLevel.values.map((level: string) => [
          level,
          JSON.stringify(reasoning.evaluate(level)),
        ]),
      ),
    };
  }),
);
writeFileSync(out, `${JSON.stringify(models, null, 2)}\n`);
runtime.dispose();
rmSync(scratch, { recursive: true, force: true });
process.exit(0);
