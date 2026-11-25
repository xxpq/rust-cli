#!/usr/bin/env node
// 独立本机 PG 的集成/故障/容量检查；不读取默认业务连接配置，不创建或删除数据库。
import { spawn } from "node:child_process";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

export function validateTestDatabase(value) {
  let url;
  try {
    url = new URL(value);
  } catch {
    throw new Error("explicit test PostgreSQL URL required");
  }
  if (
    !["postgres:", "postgresql:"].includes(url.protocol) ||
    url.hostname !== "127.0.0.1" ||
    !url.port ||
    url.pathname !== "/zcode_store_test" ||
    url.search ||
    url.hash
  ) {
    throw new Error(
      "tests require an explicit loopback port and zcode_store_test database, without URL options",
    );
  }
  return url;
}

async function cargo(args) {
  await new Promise((accept, reject) => {
    const child = spawn("cargo", args, {
      cwd: resolve(import.meta.dirname, "../../packages/rust-cli"),
      stdio: "inherit",
      env: process.env,
    });
    child.once("error", reject);
    child.once("close", (code) =>
      code === 0 ? accept() : reject(new Error(`PG check failed (exit ${code})`)),
    );
  });
}

async function main() {
  const admin = validateTestDatabase(process.env.ZCODE_TEST_POSTGRES_ADMIN_URL);
  const business = validateTestDatabase(process.env.ZCODE_TEST_POSTGRES_URL);
  if (
    admin.port !== business.port ||
    admin.username !== "postgres" ||
    business.username !== "zcode_session_test"
  )
    throw new Error("test administrator and business role must target the same isolated instance");
  if (process.argv.slice(2).some((arg) => arg !== "--load"))
    throw new Error("only --load is supported");
  for (const command of acceptanceCommands(process.argv.includes("--load"))) await cargo(command);
}

const CLI = ["test", "-p", "rust-cli", "--features", "cloud"];

export function acceptanceCommands(load) {
  const skips = load ? [] : ["--skip", "load_tests"];
  return [
    ["test", "-p", "rust-cli-pg-store", "--lib", "initialize_test_database", "--", "--ignored"],
    [
      "test",
      "-p",
      "rust-cli-pg-store",
      "--lib",
      "--",
      "--ignored",
      "--skip",
      "initialize_test_database",
      ...skips,
      "--nocapture",
    ],
    // 入口 crate 的这几步要 `cloud` 特性：缺省构建不带 PG 会话库与 serve（deployment.md §14）。
    [...CLI, "--lib", "cloud::tests", "--", "--ignored"],
    [...CLI, "--test", "pg_node_parity", "--", "--ignored"],
    [...CLI, "--test", "pg_modes", "--", "--ignored", "--nocapture"],
    [...CLI, "--test", "serve", "--", "--ignored"],
  ];
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
