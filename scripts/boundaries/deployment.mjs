// 内核不认识部署形态（docs/specs/rust-cli/architecture.md §3、pg-store.md §3）。两条规则：
//
// 1. 词汇：内核层的 crate（产品代码）里，标识符与字符串字面量不出现“云端”“租户”这类说“在哪里跑”的词。随部署形态变的
//    行为由组装根算成 `SessionPolicy`，内核只读字段；字段说的是做什么，不是在哪里跑。注释不查（注释可以解释来由）。
// 2. 端口不回答“我是哪一种实现”：快照覆盖的端口 trait 上没有 `fn is_xxx(&self) -> bool`，也没有返回 `Option<&dyn …>`
//    的方法（“有没有这个能力”）。要么行为放进端口的实现，要么拆成独立的端口由组装根注入。
//
// 2026-11-11 之前 `SessionStore` 有 `is_shared()`、`input_journal() -> Option<..>` 与 `check_delete()`，内核按返回值分支
// 8 处；这条门禁防止它们回来。纯函数，这里不做 IO：check-boundaries.mjs 收集源码与端口快照后调用；
// boundaries/deployment.test.mjs 每条规则一个合成的违规。规则表在 policy.mjs 的 DEPLOYMENT_*、PORT_KIND_METHODS。
import { isTestFile, stringLiterals, stripCode } from "./check.mjs";

/** 只在测试里编译的 Rust 文件（同 literals.mjs 的口径）。 */
const isTestOnlyFile = (file) =>
  isTestFile(file) ||
  /(_tests_\w+|\/test_support|\/test_more)\.rs$/.test(file) ||
  /\/testing(\/|\.rs$)/.test(file);

const IDENTIFIER = /[A-Za-z_][A-Za-z0-9_]*/g;

/** 字面量里按“词”找：前后不是字母数字（`gcloud` 不是 `cloud`）。 */
const asWord = (re) => new RegExp(`(?<![A-Za-z0-9])(?:${re.source})(?![A-Za-z0-9])`, re.flags);

/**
 * `sources`：`{ crate, file, text }`，路径相对 packages/rust-cli（`crates/...`）；`ports`：接口快照的 `ports`
 * （interfaces.mjs 的 `snapshot(...).ports`）。返回违规清单。
 */
export function checkDeploymentNeutral(policy, sources, ports) {
  const failures = [];
  const exempt = new Map(policy.DEPLOYMENT_TERM_EXCEPTIONS.map((entry) => [entry.file, entry]));
  const used = new Set();
  for (const { crate, file, text } of sources) {
    const layer = policy.LAYERS[crate];
    if (!policy.DEPLOYMENT_NEUTRAL_LAYERS.includes(layer) || isTestOnlyFile(file)) continue;
    const found = new Map();
    const lines = stripCode(text).split("\n");
    for (const term of policy.DEPLOYMENT_TERMS) {
      for (const [index, code] of lines.entries()) {
        for (const [identifier] of code.matchAll(IDENTIFIER)) {
          if (term.re.test(identifier) && !found.has(index + 1))
            found.set(index + 1, { word: identifier, id: term.id });
        }
      }
      const word = asWord(term.re);
      for (const { value, line } of stringLiterals(text)) {
        const match = word.exec(value);
        if (match && !found.has(line)) found.set(line, { word: match[0], id: term.id });
      }
    }
    if (!found.size) continue;
    if (exempt.has(file)) {
      used.add(file);
      continue;
    }
    for (const [line, { word, id }] of [...found].sort(([a], [b]) => a - b)) {
      failures.push(
        `${file}:${line}: "${word}" names the deployment (${id}); ${crate} (${layer}) must not know where it runs: put the behaviour in SessionPolicy or behind a port (architecture.md §3)`,
      );
    }
  }
  for (const { file, reason } of policy.DEPLOYMENT_TERM_EXCEPTIONS) {
    if (!used.has(file))
      failures.push(
        `DEPLOYMENT_TERM_EXCEPTIONS: ${file} has no deployment term (dead entry: ${reason})`,
      );
  }
  for (const [name, port] of Object.entries(ports ?? {})) {
    for (const { signature } of port?.methods ?? []) {
      for (const rule of policy.PORT_KIND_METHODS) {
        if (rule.re.test(signature))
          failures.push(
            `interfaces: ${name}: \`${signature}\` asks the implementation what kind it is (${rule.id}); move the behaviour into the port, or split it into its own port that the composition root injects (architecture.md §3)`,
          );
      }
    }
  }
  return failures;
}
