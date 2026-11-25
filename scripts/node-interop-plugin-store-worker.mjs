// rust-cli 与 Node 共写插件存储的 Node 一侧（docs/specs/rust-cli/plugin-store.md §14、§16；node-interop-plugin-store.mjs）。
//
// 由 node-interop-plugin-store.mjs 以 tsx 子进程启动，跑一个 Node 的真实插件写（bootstrap 的 `pluginMutation` 入口：持 Node 的
// 存储锁），把结果写成一行 JSON。单独的进程：并发与「半路被杀」要两个进程。HOME 由父进程给（临时目录）。
//
// 用法：node --import <tsx loader> node-interop-plugin-store-worker.mjs --node-root <检出> <命令> <JSON 参数>
//   add {source}                 addZCodePluginMarketplace
//   install {marketplace, name}  installZCodeMarketplacePlugin
//   dead-tx {target, source, authority, marketplace}
//                                Node 的原子目录激活 + 权威记录写入事务 id，然后不收尾就退出（模拟 Node 在提交之后、收尾之前崩溃）
import { readFile, writeFile } from "node:fs/promises";
import { importNode, nodeRootFromArgs } from "./node-root.mjs";

const { nodeRoot, args } = nodeRootFromArgs(process.argv.slice(2));
const [command, raw] = args;
const input = JSON.parse(raw ?? "{}");
const base = { workingDirectory: process.cwd(), env: process.env };

if (command === "dead-tx") {
  const { activateDirectoryAtomically } = await importNode(
    nodeRoot,
    "adapters/src/plugins/atomic-directory.ts",
  );
  const activation = await activateDirectoryAtomically({
    authorityPath: input.authority,
    sourcePath: input.source,
    targetPath: input.target,
  });
  const known = JSON.parse(await readFile(input.authority, "utf8"));
  for (const record of known.marketplaces) {
    if (record.id === input.marketplace) record.cacheTransactionId = activation.transactionId;
  }
  await writeFile(input.authority, `${JSON.stringify(known, null, 2)}\n`);
  process.stdout.write(`${JSON.stringify({ transactionId: activation.transactionId })}\n`);
  // 不调 activation.finalize()：备份与事务文件留在磁盘上，owner 是这个马上就退出的进程。
  process.exit(0);
}

const plugins = await importNode(nodeRoot, "bootstrap/src/plugins.ts");
let result;
if (command === "add") {
  result = await plugins.addZCodePluginMarketplace({ ...base, source: input.source });
} else if (command === "install") {
  result = await plugins.installZCodeMarketplacePlugin({
    ...base,
    marketplace: input.marketplace,
    pluginName: input.name,
  });
} else {
  throw new Error(`unknown command ${command}`);
}
process.stdout.write(`${JSON.stringify(result)}\n`);
