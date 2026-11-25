// node-interop 的入口不在真实用户目录里跑（docs/specs/rust-cli/node-store.md §6.1；docs/zcode-protocol-v5/10-restart-assessment.md
// §5.2 第 ⑥ 项）：HOME 是账户的家目录、或 `ZCODE_*` 的路径落在它里面时，入口什么都不做、以 2 退出。
//
// 起入口的用例给一个不存在的 `--node-root`：没有这道检查时入口在「没有装依赖的检出」处同样以 2 退出、不构建也不导入 Node 的代码，
// 所以这些用例在旧代码上安全地失败（错误信息不同）。
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir, userInfo } from "node:os";
import { join } from "node:path";
import { after, before, describe, test } from "node:test";
import { realHomeProblems } from "./node-interop-home.mjs";
import { EXIT_UNUSABLE_ENVIRONMENT } from "./node-root.mjs";

const REAL = "/Users/dev";
const REFUSED = "refusing to run against the real user home";
/** 能单独运行的互操作入口：总入口与两段自带入口的。 */
const ENTRIES = ["node-interop-check.mjs", "node-interop-runtime.mjs", "node-interop-race.mjs"];

describe("realHomeProblems", () => {
  test("HOME 是账户的家目录", () => {
    const problems = realHomeProblems({ env: {}, home: REAL, realHome: REAL });
    assert.equal(problems.length, 1);
    assert.match(problems[0], /^HOME /);
  });

  test("ZCODE_* 的路径是家目录或在它里面", () => {
    const problems = realHomeProblems({
      env: {
        ZCODE_DATA_BASE_DIR: REAL,
        ZCODE_SESSION_DB_PATH: join(REAL, ".zcode/cli/db/db.sqlite"),
      },
      home: "/tmp/interop-home",
      realHome: REAL,
    });
    assert.deepEqual(
      problems.map((problem) => problem.split(" ")[0]),
      ["ZCODE_DATA_BASE_DIR", "ZCODE_SESSION_DB_PATH"],
    );
  });

  test("临时目录、不是路径的值、源码检出、名字相近的目录都不算", () => {
    const problems = realHomeProblems({
      env: {
        ZCODE_DATA_BASE_DIR: "/tmp/interop-home",
        ZCODE_API_KEY: "test-key",
        ZCODE_ENV: "test",
        ZCODE_NODE_ROOT: join(REAL, "code/z-code"),
        ZCODE_STORAGE_DIR: "/Users/developer/storage",
        HOME_COPY: REAL,
      },
      home: "/tmp/interop-home",
      realHome: REAL,
    });
    assert.deepEqual(problems, []);
  });
});

describe("入口拒绝真实 HOME", () => {
  let scratch;
  const realHome = userInfo().homedir;
  /** 本进程的环境去掉 `ZCODE_*`（调用方的 shell 可能设了），再换上用例的。 */
  const envWith = (extra) => ({
    ...Object.fromEntries(
      Object.entries(process.env).filter(([name]) => !name.startsWith("ZCODE_")),
    ),
    ...extra,
  });
  const run = (entry, env) =>
    spawnSync(
      process.execPath,
      [join(import.meta.dirname, entry), "--node-root", join(scratch, "missing-checkout")],
      { env, encoding: "utf8" },
    );

  before(async () => {
    scratch = await mkdtemp(join(tmpdir(), "rust-cli-node-interop-home-"));
  });
  after(async () => {
    await rm(scratch, { recursive: true, force: true });
  });

  for (const entry of ENTRIES) {
    test(`${entry}：HOME 是账户的家目录`, () => {
      const result = run(entry, envWith({ HOME: realHome, USERPROFILE: realHome }));
      assert.equal(result.status, EXIT_UNUSABLE_ENVIRONMENT, result.stderr);
      assert.match(result.stderr, new RegExp(REFUSED));
    });

    test(`${entry}：ZCODE_DATA_BASE_DIR 指向账户的家目录`, () => {
      const home = join(scratch, "home");
      const result = run(
        entry,
        envWith({ HOME: home, USERPROFILE: home, ZCODE_DATA_BASE_DIR: realHome }),
      );
      assert.equal(result.status, EXIT_UNUSABLE_ENVIRONMENT, result.stderr);
      assert.match(result.stderr, new RegExp(REFUSED));
      assert.match(result.stderr, /ZCODE_DATA_BASE_DIR/);
    });
  }
});
