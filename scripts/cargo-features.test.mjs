// cargo 的 feature 参数：仓库里的检查与测试总带 `cloud`（云端的部件也要过门禁），有 parity 数据时再加 `parity`。
import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { featureArgs, featureList } from "./cargo-features.mjs";

test("cloud is always on; parity follows the data directory", () => {
  assert.deepEqual(featureList({ parity: false }), ["cloud"]);
  assert.deepEqual(featureList({ parity: true }), ["cloud", "parity"]);
});

test("the arguments name the features of the tree they are run in", async () => {
  const workspace = await mkdtemp(join(tmpdir(), "cargo-features-"));
  try {
    assert.deepEqual(await featureArgs(workspace), ["--features", "cloud"]);
    await mkdir(join(workspace, "tests/parity"), { recursive: true });
    assert.deepEqual(await featureArgs(workspace), ["--features", "cloud,parity"]);
  } finally {
    await rm(workspace, { recursive: true, force: true });
  }
});
