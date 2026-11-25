#!/usr/bin/env node
// 工作区 MCP 信任 digest 的 Node 对照（docs/zcode-protocol-v5/18-mcp-trust.md §12.2）。
//
// digest 里有绝对路径（工作区、来源文件、stdio 的 cwd），所以按夹具
// `packages/rust-cli/crates/cli/tests/fixtures/workspace-mcp.json` 在固定的目录 `/tmp/zcode-mcp-digest-golden/c<序号>/` 下摆出文件，
// 用 Node 的 `createWorkspaceMcpTrust(...).inspect()`（apps/zcode-cli/packages/bootstrap/src/workspace-mcp-trust/index.ts）算出每个
// 工作区声明的 digest，写进同目录的 `workspace-mcp-golden.json`；`--check` 只比较。Rust 测试（crates/cli/src/config/mcp_trust_tests.rs）
// 用同一组路径在内存里摆出同样的文件、走生产代码逐项比较。用完删掉摆出的目录。只在 POSIX 上跑（路径是 POSIX 的）。
//
// `platform: "win32"` 的一例：Node 的 `normalizeMcpStdioCommand` 在调用时看 `process.platform`，脚本在这一例里把它换成 `win32`
// （路径函数在加载时已定，仍是 POSIX），只验证 `cmd /c` 的拆法。
//
// 用法：node scripts/rust-cli/mcp-digest-golden.mjs [--check] [--node-root <装好依赖的检出>]
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { isDeepStrictEqual } from "node:util";
import {
  ensureTypeScript,
  importNode,
  nodeRootFromArgs,
  repoRoot,
  withHome,
} from "./node-root.mjs";

/** 摆文件的固定根目录：digest 含绝对路径，换目录 digest 就变。 */
const ROOT = "/tmp/zcode-mcp-digest-golden";

const { nodeRoot, args } = nodeRootFromArgs(process.argv.slice(2));
ensureTypeScript(import.meta.filename, nodeRoot, args);
const check = args.includes("--check");

const fixtures = join(repoRoot, "packages/rust-cli/crates/cli/tests/fixtures");
const cases = JSON.parse(readFileSync(join(fixtures, "workspace-mcp.json"), "utf8"));
const { createWorkspaceMcpTrust } = await importNode(
  nodeRoot,
  "bootstrap/src/workspace-mcp-trust/index.ts",
);

function put(path, content) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, typeof content === "string" ? content : JSON.stringify(content));
}

/** `process.platform` 换成 `platform` 跑 `run`，之后换回来（只有一例要它）。 */
async function onPlatform(platform, run) {
  if (!platform) return run();
  const saved = Object.getOwnPropertyDescriptor(process, "platform");
  Object.defineProperty(process, "platform", { value: platform });
  try {
    return await run();
  } finally {
    Object.defineProperty(process, "platform", saved);
  }
}

rmSync(ROOT, { recursive: true, force: true });
const results = [];
try {
  for (const [index, testCase] of cases.entries()) {
    const base = join(ROOT, `c${index}`);
    const home = join(base, "home");
    const userConfigPath = join(home, ".zcode/cli/config.json");
    if (testCase.user) put(userConfigPath, testCase.user);
    // `home`：home 下的其他文件（用户级的 `~/.agents/mcp.json`，18-mcp-trust.md §14.10），路径相对 home
    for (const [path, content] of Object.entries(testCase.home ?? {}))
      put(join(home, path), content);
    if (testCase.git) mkdirSync(join(base, testCase.git, ".git"), { recursive: true });
    for (const [path, content] of Object.entries(testCase.files)) put(join(base, path), content);
    const reviews = await withHome(home, () =>
      onPlatform(testCase.platform, () =>
        createWorkspaceMcpTrust({
          workingDirectory: join(base, testCase.workspace),
          userConfigPath,
          env: {},
        }).inspect(),
      ),
    );
    const servers = Object.fromEntries(
      Object.keys(reviews)
        .sort()
        .map((name) => [
          name,
          {
            declarationDigest: reviews[name].declarationDigest,
            configuredEnabled: reviews[name].configuredEnabled,
            sourcePaths: reviews[name].sourcePaths,
          },
        ]),
    );
    results.push({ name: testCase.name, servers });
  }
} finally {
  rmSync(ROOT, { recursive: true, force: true });
}

const golden = join(fixtures, "workspace-mcp-golden.json");
if (check) {
  const current = JSON.parse(readFileSync(golden, "utf8"));
  if (!isDeepStrictEqual(current, results)) {
    console.error("workspace-mcp-golden.json is out of date; rerun without --check");
    process.exit(1);
  }
  console.log("mcp-digest-golden: OK");
} else {
  writeFileSync(golden, `${JSON.stringify(results, null, 2)}\n`);
  console.log(`wrote ${golden}`);
}
