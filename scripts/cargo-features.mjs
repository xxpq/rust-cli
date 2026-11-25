// cargo 的 feature 参数。test.mjs 与 clippy.mjs 共用。
//
// - `cloud`（入口 crate，docs/specs/rust-cli/deployment.md §14）：云端的部件（PG 会话库、`zcode-rs serve`）。缺省构建不带
//   它（本机发的 zcode 不链接 PG 驱动），但仓库里的检查与测试总带上：云端的代码同样要过门禁。缺省构建由 clippy.mjs
//   另查一遍。
// - `parity`（docs/specs/rust-cli/export.md §5）：parity 测试的数据目录 packages/rust-cli/tests/parity 在时（完整的源码
//   仓库）打开，不在时（只有产品代码的树）不打开。
import { access } from "node:fs/promises";
import { resolve } from "node:path";

/** 要打开的 feature（按名字排好）。 */
export function featureList({ parity }) {
  return parity ? ["cloud", "parity"] : ["cloud"];
}

/** `workspace` 是 packages/rust-cli。 */
export async function featureArgs(workspace) {
  let parity = true;
  try {
    await access(resolve(workspace, "tests/parity"));
  } catch {
    parity = false;
  }
  return ["--features", featureList({ parity }).join(",")];
}
