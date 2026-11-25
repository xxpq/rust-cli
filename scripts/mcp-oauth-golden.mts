#!/usr/bin/env node
// rust-cli 远程 MCP 的 OAuth 与 Node 互通的对照夹具（docs/specs/rust-cli/ext.md §3.12.4）：用 Node 适配器的真代码算键的前缀、
// 刷新锁的路径与 canonical pair 的派生，本工程的测试逐项比较。纯函数，不连网、不读写文件。
//
//   node_modules/.bin/tsx scripts/rust-cli/mcp-oauth-golden.mts \
//     packages/rust-cli/crates/platform/tests/fixtures/mcp-oauth-golden.json
//
// 两部分：
// - prefixes：`createCredentialKeyPrefix`（`oauth.ts`）与 `resolveRefreshLockPath`（`oauth-refresh.ts`）；
// - pairs：`deriveCredentialPair`（`oauth-credentials.ts`）对 canonical v1 / v2、legacy 镜像各种组合的结论。
import { writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const [outFile] = process.argv.slice(2);
if (!outFile) throw new Error("usage: mcp-oauth-golden.mts <out.json>");
const root = resolve(import.meta.dirname, "../..");
const adapters = (file: string) =>
  pathToFileURL(resolve(root, "apps/zcode-cli/packages/adapters/src/mcp", file)).href;

const { createCredentialKeyPrefix } = await import(adapters("oauth.ts"));
const { resolveRefreshLockPath } = await import(adapters("oauth-refresh.ts"));
const { deriveCredentialPair } = await import(adapters("oauth-credentials.ts"));

const CREDENTIALS = "/Users/dev/.zcode/v2/credentials.json";
const prefixCases: [string, string, Record<string, string>][] = [
  ["docs", "https://mcp.example.com/mcp", {}],
  [
    "plugin:kit:canva",
    "https://mcp.example.com/sse?x=1",
    { clientId: "c1", scope: "a b", redirectPath: "/cb" },
  ],
  ["中文 名", "http://127.0.0.1:8080/mcp", { scope: "files:read" }],
];
const prefixes = prefixCases.map(([server, url, oauth]) => {
  const prefix = createCredentialKeyPrefix(server, url, { type: "authorization_code", ...oauth });
  return { server, url, oauth, prefix, lock: resolveRefreshLockPath(CREDENTIALS, prefix) };
});

const client = { client_id: "dyn-1", redirect_uris: ["http://127.0.0.1:1/cb"] };
const otherClient = { client_id: "dyn-2" };
const tokens = {
  access_token: "at-1",
  token_type: "Bearer",
  expires_in: 3600,
  refresh_token: "rt-1",
};
const newer = { access_token: "at-2", token_type: "Bearer", refresh_token: "rt-2" };
const canonical = (version: number, extra: Record<string, unknown> = {}) =>
  JSON.stringify({
    client_information: client,
    expires_at: 1_700_003_600_000,
    generation: "0123456789abcdef0123456789abcdef",
    issuer: "https://auth.example.com",
    obtained_at: 1_700_000_000_000,
    published_by: "tx",
    tokens,
    version,
    ...extra,
  });
const legacy = (value: unknown) => (value === undefined ? undefined : JSON.stringify(value));
const pairCases: [string, string | undefined, unknown, unknown][] = [
  ["nothing", undefined, undefined, undefined],
  ["legacy only", undefined, client, tokens],
  ["legacy client only", undefined, client, undefined],
  ["v2 with matching mirrors", canonical(2), client, tokens],
  ["v2 without mirrors (old process invalidated)", canonical(2), undefined, undefined],
  ["v2, legacy tokens gone", canonical(2), client, undefined],
  ["v2, same client, legacy refreshed", canonical(2), client, newer],
  ["v2, legacy client and tokens both changed", canonical(2), otherClient, newer],
  ["v2, tokens same, legacy client gone", canonical(2), undefined, tokens],
  ["v1 without mirrors", canonical(1), undefined, undefined],
  ["v1, legacy refreshed by an old process", canonical(1), client, newer],
  ["v1, legacy from another client", canonical(1), otherClient, newer],
  ["v2 without generation", canonical(2, { generation: undefined }), client, tokens],
  ["unknown version", canonical(3), client, tokens],
  ["broken canonical", "{not json", client, tokens],
];
const pairs = pairCases.map(([name, canonicalRaw, legacyClient, legacyTokens]) => {
  const input = {
    canonicalRaw,
    legacyClientRaw: legacy(legacyClient),
    legacyTokensRaw: legacy(legacyTokens),
  };
  const derived = deriveCredentialPair(input);
  return {
    name,
    input,
    // 本工程比 client、令牌与 canonical 原文（source 是 Node 内部的分支名，不比）
    expected:
      derived === undefined
        ? null
        : {
            client: derived.clientInformation ?? null,
            tokens: derived.tokens ?? null,
            raw: derived.raw ?? null,
            generation: derived.generation ?? null,
          },
  };
});

writeFileSync(
  outFile,
  `${JSON.stringify({ credentials: CREDENTIALS, prefixes, pairs }, null, 2)}\n`,
);
