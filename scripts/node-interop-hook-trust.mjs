// rust-cli 与 Node 共用工作区 hooks 的预信任记录（HOST-PORT-V5，docs/specs/rust-cli/config.md §4.2、§4.5；node-interop-check.mjs 的第三十六段）。
//
// App 的 v5 `hooks.trustGrant` 与 `zcode hooks trust grant` 是同一段代码（config.md §4.5），这里用真实的 `zcode-rs hooks trust` 写与读：
// - 两边对同一份项目配置算出同一个 bundle 与同一组声明 digest；
// - rust 写、Node 读：Node 的 `inspectWorkspaceHookTrust` 把 rust 信任的那一条读成 trusted_persistent，另一条仍是 pending_trust；
// - Node 写、rust 读：Node 的 `grantWorkspaceHookTrust` 信任另一条，rust 的 `zcode-rs hooks trust status --json` 两条都读成 trusted_persistent。
// 关开关回到 Node 之后，在 rust 下授权过的 hook 照样跑（反过来也一样）。只用临时 HOME。
import { execFile } from "node:child_process";
import { realpathSync } from "node:fs";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { promisify } from "node:util";
import { importNode, withHome } from "./node-root.mjs";

const TRUSTED = "trusted_persistent";
const PENDING = "pending_trust";

/** 两条项目级 UserPromptSubmit hook（exec 形式，不经 shell）。 */
const PROJECT_HOOKS = {
  hooks: {
    enabled: true,
    events: {
      UserPromptSubmit: [
        {
          hooks: [
            { type: "process", command: "node", args: ["first.mjs"] },
            { type: "process", command: "node", args: ["second.mjs"] },
          ],
        },
      ],
    },
  },
};

export async function checkHookTrust({ nodeRoot, base, expectEqual, zcode }) {
  const { inspectWorkspaceHookTrust, grantWorkspaceHookTrust } = await importNode(
    nodeRoot,
    "bootstrap/src/workspace-hook-trust-cli.ts",
  );
  const home = join(base, "home");
  await mkdir(home, { recursive: true });
  await mkdir(join(base, "work", ".git"), { recursive: true });
  await mkdir(join(base, "work", ".zcode"), { recursive: true });
  // 工作区身份是工作区根的绝对路径（macOS 的临时目录经符号链接解析成 /private/…，两边都用解析之后的）
  const work = realpathSync(join(base, "work"));
  await writeFile(
    join(work, ".zcode", "config.json"),
    `${JSON.stringify(PROJECT_HOOKS, null, 2)}\n`,
  );
  const env = { HOME: home, USERPROFILE: home, PATH: process.env.PATH };
  const rust = async (...args) => {
    const { stdout } = await promisify(execFile)(zcode, ["hooks", "trust", ...args], {
      cwd: work,
      env,
    });
    return stdout;
  };
  const rustStatus = async () => JSON.parse(await rust("status", "--workspace", work, "--json"));
  const node = (run) => withHome(home, run);

  const before = await rustStatus();
  const nodeBefore = await node(() => inspectWorkspaceHookTrust({ workspacePath: work }));
  expectEqual(
    "rust and Node compute the same bundle digest",
    before.bundleDigest,
    nodeBefore.bundleDigest,
  );
  expectEqual(
    "rust and Node compute the same declaration digests",
    before.items.map((item) => item.hookDeclarationDigest),
    nodeBefore.items.map((item) => item.hookDeclarationDigest),
  );
  const [first, second] = before.items.map((item) => item.hookDeclarationDigest);

  // rust 写、Node 读
  await rust(
    "grant",
    "--workspace",
    work,
    "--bundle-digest",
    before.bundleDigest,
    "--hook-digest",
    first,
  );
  const nodeAfterRust = await node(() => inspectWorkspaceHookTrust({ workspacePath: work }));
  expectEqual(
    "Node reads the declaration rust trusted as trusted and the other as pending",
    nodeAfterRust.items.map((item) => item.trustState),
    [TRUSTED, PENDING],
  );

  // Node 写、rust 读
  await node(() =>
    grantWorkspaceHookTrust({
      workspacePath: work,
      bundleDigest: before.bundleDigest,
      hookDeclarationDigests: [second],
    }),
  );
  const rustAfterNode = await rustStatus();
  expectEqual(
    "rust reads both declarations as trusted after Node trusts the second",
    rustAfterNode.items.map((item) => item.trustState),
    [TRUSTED, TRUSTED],
  );
  expectEqual(
    "rust reads the workspace as trusted",
    rustAfterNode.reasonCode,
    "workspace_hooks_trusted_persistent",
  );
}
