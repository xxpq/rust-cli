// node-interop 的入口不在真实用户目录里跑（docs/specs/rust-cli/node-store.md §6.1）：总入口与两段自带的入口最先调 `refuseRealHome`。
//
// 修 bug（EXP-DEFECTS-1，docs/zcode-protocol-v5/10-restart-assessment.md §5.2 第 ⑥ 项）：以前各段自己把 HOME 指向临时目录
// （`withHome`、子进程的 env），运行器本身用调用方的 HOME。一段在 `withHome` 之外调了按 `os.homedir()` 找库的 Node 函数，
// 打开过本机真实的 `~/.zcode/cli/db/db.sqlite`（跑了 Node 的迁移检查）。现在入口先查：HOME 是真实的家目录、或 `ZCODE_*`
// 的路径落在真实的家目录里，就什么都不做、直接退出；调用方给临时 HOME。
import { homedir, userInfo } from "node:os";
import { isAbsolute, relative, resolve } from "node:path";
import { EXIT_UNUSABLE_ENVIRONMENT } from "./node-root.mjs";

/** 互操作脚本认的环境变量前缀：其中值是绝对路径的，都是 Node 或本工程读写数据的位置。 */
const ZCODE_ENV_PREFIX = "ZCODE_";
/** `ZCODE_*` 里不是数据位置的：装好依赖的源码检出（只读，常在用户目录里）。 */
const SOURCE_CHECKOUT_VARS = new Set(["ZCODE_NODE_ROOT"]);

/**
 * 运行器不能碰真实用户目录的理由（空表 = 可以跑）。`home` 是本进程的 `os.homedir()`，`realHome` 是账户的家目录
 * （`os.userInfo().homedir`，不看 HOME / USERPROFILE）。
 */
export function realHomeProblems({ env, home, realHome }) {
  const real = resolve(realHome);
  const inside = (path) => {
    const rest = relative(real, resolve(path));
    return rest === "" || (!rest.startsWith("..") && !isAbsolute(rest));
  };
  const problems = [];
  if (resolve(home) === real) problems.push(`HOME is the real user home (${real})`);
  for (const [name, value] of Object.entries(env)) {
    if (!name.startsWith(ZCODE_ENV_PREFIX) || SOURCE_CHECKOUT_VARS.has(name)) continue;
    if (value && isAbsolute(value) && inside(value))
      problems.push(`${name} points into the real user home (${value})`);
  }
  return problems;
}

/**
 * 互操作脚本的入口最先调它（在构建、导入 Node 的代码之前）：有 `realHomeProblems` 时打印理由与用法，以
 * `EXIT_UNUSABLE_ENVIRONMENT` 退出。cargo 要的工具链位置由 `CARGO_HOME`、`RUSTUP_HOME` 给（不再跟着 HOME）。
 */
export function refuseRealHome(script) {
  const problems = realHomeProblems({
    env: process.env,
    home: homedir(),
    realHome: userInfo().homedir,
  });
  if (problems.length === 0) return;
  console.error(
    `${script}: refusing to run against the real user home:\n` +
      problems.map((problem) => `  - ${problem}\n`).join("") +
      "Run it with a temporary HOME and explicit toolchain locations, e.g.\n" +
      '  HOME="$(mktemp -d)" CARGO_HOME=<cargo home> RUSTUP_HOME=<rustup home> node <script>',
  );
  process.exit(EXIT_UNUSABLE_ENVIRONMENT);
}
