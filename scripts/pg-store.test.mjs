import assert from "node:assert/strict";
import test from "node:test";
import { acceptanceCommands, validateTestDatabase } from "./pg-store.mjs";

test("PG integration runner accepts only an isolated loopback database", () => {
  assert.equal(
    validateTestDatabase("postgres://postgres@127.0.0.1:64321/zcode_store_test").port,
    "64321",
  );
  for (const url of [
    undefined,
    "postgres://admin@production.example.com/zcode_store_test",
    "postgres://admin@127.0.0.1/business",
    "http://127.0.0.1/zcode_store_test",
    "postgres://admin@127.0.0.1/zcode_store_test?options=unsafe",
  ]) {
    assert.throws(() => validateTestDatabase(url));
  }
});

test("acceptance always runs real local and remote modes", () => {
  const commands = acceptanceCommands(false);
  assert(commands.some((args) => args.includes("pg_modes") && args.includes("--ignored")));
  assert(commands.some((args) => args.includes("cloud::tests")));
  // 本机 SQLite 与云 PG 写出相同的 Node 行（pg-store.md §12.3）。
  assert(commands.some((args) => args.includes("pg_node_parity") && args.includes("--ignored")));
  assert(commands.some((args) => args.includes("initialize_test_database")));
  // serve：多个用户的轮次共用一个进程与一个 PG 连接池（multi-tenant.md §8.1）。
  assert(commands.some((args) => args.includes("serve") && args.includes("--ignored")));
  assert(commands.some((args) => args.includes("--skip") && args.includes("load_tests")));
  assert(!acceptanceCommands(true).some((args) => args.includes("load_tests")));
  // 入口 crate 的这几步要 `cloud` 特性（缺省构建不带 PG 会话库与 serve）。
  for (const args of commands.filter((args) => args[2] === "rust-cli")) {
    const at = args.indexOf("--features");
    assert(at > 0 && args[at + 1] === "cloud" && at < args.indexOf("--"), args.join(" "));
  }
});
