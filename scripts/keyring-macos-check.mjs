#!/usr/bin/env node
// macOS 真实钥匙串的手动核对（docs/specs/rust-cli/platform.md §13.10，CRED-V2）：rust 的 keyring crate 与 Node 的
// `@napi-rs/keyring` 读写同一个钥匙串条目。不进自动测试。
//
//   node scripts/rust-cli/keyring-macos-check.mjs
//
// 只碰一个新建的临时钥匙串。父进程建临时目录，整个核对在子进程里跑，子进程的 HOME 是临时目录：钥匙串的用户域按 HOME 找
// 搜索列表与缺省钥匙串，`@napi-rs/keyring` 也在这个子进程里，所以 Node 一侧同样只看临时钥匙串。修 bug（ISSUES
// 2026-11-11 CRED-V2）：第一版只给 `security` 与 rust 的子进程设了临时 HOME，`@napi-rs/keyring` 在脚本自己的进程里按真实
// HOME 写进了登录钥匙串。子进程动手之前再核对三件事：自己的 HOME 是临时目录；这个 HOME 下原来没有缺省钥匙串；建好之后用户域的
// 搜索列表与缺省钥匙串都在临时目录里。任何一条不成立就停下，不写任何条目。服务名是测试专用的。子进程最后
// `security delete-keychain` 删掉这个临时钥匙串（COMMON「碰钥匙串」：钥匙串的调用都在核对过的子进程里），父进程只删临时
// 目录。rust 一侧（`keyring_probe`）关掉钥匙串的用户交互：要弹框的访问报错，不在桌面上弹框。
//
// 1. 用 `security add-generic-password -A` 建一个访问控制放开的条目：Node 与 rust 读出同一个值；
// 2. rust 写一个条目：rust 读回同一个值，`security` 按服务名与账号名找得到它（只看属性，不读值）；
// 3. Node 写一个条目（只信任 Node）：rust 读它时系统要问用户——交互关着，报错而不是弹框（spec 里的“第一次读时弹框”）；
// 4. 没有的条目：rust 与 Node 都读出 null。
import { execFile } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { mkdir, mkdtemp, realpath, rm } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";

const run = promisify(execFile);
const repoRoot = resolve(import.meta.dirname, "../..");
const probe = join(repoRoot, "packages/rust-cli/target/debug/examples/keyring_probe");
/** 测试专用的服务名（产品是 `ZCode Credential Encryption`）。 */
const SERVICE = "ZCode Credential Encryption Test";
const INNER = "--inner";

if (process.platform !== "darwin") {
  console.error("keyring-macos-check: macOS only");
  process.exit(2);
}

const keychainOf = (home) => join(home, "Library/Keychains/login.keychain-db");
const security = (...args) => run("/usr/bin/security", args, { env: process.env });
const exitOf = (promise) =>
  promise.then(
    (result) => ({ code: 0, stdout: result.stdout }),
    (error) => ({ code: error.code ?? 1, stdout: error.stdout ?? "", stderr: error.stderr ?? "" }),
  );

/** 一个程序在钥匙串分区列表里的名字：Developer ID 签名的是 `teamid:<团队>`，否则是 `cdhash:<代码目录的哈希>`。 */
async function partitionOf(binary) {
  const { stderr } = await run("/usr/bin/codesign", ["-dv", "--verbose=4", binary]);
  const team = /^TeamIdentifier=(\S+)$/m.exec(stderr)?.[1];
  if (team && team !== "not") return `teamid:${team}`;
  const cdhash = /^CDHash=([0-9a-f]+)$/m.exec(stderr)?.[1];
  if (!cdhash) throw new Error(`no code signature on ${binary}`);
  return `cdhash:${cdhash}`;
}

/** 用户域的缺省钥匙串（按这个进程的 HOME）在不在临时目录里；输出不打印。 */
async function defaultInHome(home) {
  const result = await exitOf(security("default-keychain", "-d", "user"));
  return { code: result.code, inside: result.stdout.includes(home) };
}

/** 父进程：临时目录、子进程、清理。 */
async function outer() {
  const base = await realpath(await mkdtemp(join(tmpdir(), "rust-cli-keychain-")));
  const home = join(base, "home");
  await mkdir(join(home, "Library/Keychains"), { recursive: true });
  const env = { HOME: home, PATH: process.env.PATH };
  try {
    const child = await exitOf(
      run(process.execPath, [import.meta.filename, INNER, home], { env, maxBuffer: 1 << 20 }),
    );
    process.stdout.write(child.stdout);
    if (child.code !== 0) process.stderr.write(`${child.stderr}\n`);
    process.exitCode = child.code === 0 ? 0 : 1;
  } finally {
    await rm(base, { recursive: true, force: true });
  }
}

/** 子进程：HOME 是临时目录。 */
async function inner(home) {
  if (process.env.HOME !== home || homedir() !== home) {
    throw new Error("the check must run with HOME set to its temporary directory; stopping");
  }
  const keychain = keychainOf(home);
  // 开始前：这个 HOME 下没有缺省钥匙串（钥匙串的用户域确实按 HOME 找）。输出不打印。
  const before = await defaultInHome(home);
  if (before.code === 0 && !before.inside) {
    throw new Error(
      "the keychain user domain does not follow HOME here; stopping before any change",
    );
  }
  const password = randomBytes(16).toString("hex");
  await security("create-keychain", "-p", password, keychain);
  try {
    await checks(home, keychain, password);
  } finally {
    // 只删这个临时钥匙串（路径在临时目录里），同时从临时 HOME 的搜索列表里去掉
    await exitOf(security("delete-keychain", keychain));
  }
}

/** 子进程里、临时钥匙串建好之后的核对。 */
async function checks(home, keychain, password) {
  // 建好之后：用户域的搜索列表里只有临时目录里的钥匙串（写进的是临时 HOME 的偏好）。输出不打印。
  const listed = (await security("list-keychains", "-d", "user")).stdout
    .split("\n")
    .map((line) => line.trim().replace(/^"|"$/g, ""))
    .filter(Boolean);
  if (!listed.length || listed.some((path) => !path.startsWith(home))) {
    throw new Error("the keychain search list is not the temporary one; stopping");
  }
  await security("unlock-keychain", "-p", password, keychain);
  await security("set-keychain-settings", keychain);
  if (!(await defaultInHome(home)).inside)
    await security("default-keychain", "-d", "user", "-s", keychain);
  if (!(await defaultInHome(home)).inside) {
    throw new Error("the temporary keychain is not the default one; stopping");
  }

  const failures = [];
  const expect = (label, ok, detail = "") => {
    console.log(`${ok ? "ok  " : "FAIL"} ${label}${ok || !detail ? "" : `: ${detail}`}`);
    if (!ok) failures.push(label);
  };
  expect("the default keychain and the search list are the temporary ones", true);
  const account = (name) =>
    createHash("sha256").update(`/Users/dev/.zcode/v2/${name}`).digest("hex");
  const key = randomBytes(32).toString("base64url");
  const rust = async (...args) =>
    JSON.parse((await run(probe, args, { env: process.env })).stdout.trim().split("\n").at(-1));
  const { AsyncEntry } = await import(
    pathToFileURL(join(repoRoot, "node_modules/@napi-rs/keyring/index.js")).href
  );
  const nodeRead = (name) => new AsyncEntry(SERVICE, account(name)).getPassword();

  // 1. 访问控制放开的条目（相当于用户点过“始终允许”）：两边读出同一个值。`-A` 放开旧式的访问控制，分区列表另外放进
  //    Node 与 rust 这两个程序（Developer ID 签名的是 `teamid:`，ad-hoc 签名的是 `cdhash:`）。
  const sharedArgs = ["-A", "-s", SERVICE, "-a", account("shared"), "-w", key, keychain];
  await security("add-generic-password", ...sharedArgs);
  const partitions = [
    "apple-tool:",
    "apple:",
    await partitionOf(process.execPath),
    await partitionOf(probe),
  ];
  const partitionArgs = ["-S", partitions.join(","), "-k", password];
  await security(
    "set-generic-password-partition-list",
    ...partitionArgs,
    "-s",
    SERVICE,
    "-a",
    account("shared"),
    keychain,
  );
  expect("Node reads the shared item", (await nodeRead("shared")) === key);
  const shared = await rust("read", SERVICE, account("shared"));
  expect("rust reads the shared item", shared.value === key, JSON.stringify(shared));

  // 2. rust 写：rust 读回；条目的服务名与账号名对（只看属性）。
  const written = await rust("write", SERVICE, account("rust"), key);
  expect("rust writes an item", written.ok === true, JSON.stringify(written));
  const back = await rust("read", SERVICE, account("rust"));
  expect("rust reads its item back", back.value === key, JSON.stringify(back));
  const found = await exitOf(
    security("find-generic-password", "-s", SERVICE, "-a", account("rust"), keychain),
  );
  expect("the item rust wrote has the Node service and account", found.code === 0);

  // 3. Node 写的条目只信任 Node：rust 读它要用户同意，交互关着时报错（不弹框）。
  await new AsyncEntry(SERVICE, account("node")).setPassword(key);
  const guarded = await rust("read", SERVICE, account("node"));
  expect(
    "rust reading a Node item needs the user's consent",
    typeof guarded.error === "string",
    JSON.stringify(guarded),
  );

  // 4. 没有的条目。
  expect(
    "rust reads a missing item as null",
    (await rust("read", SERVICE, account("missing"))).value === null,
  );
  expect("Node reads a missing item as null", (await nodeRead("missing")) === null);
  console.log(
    failures.length
      ? `keyring-macos-check: ${failures.length} failure(s)`
      : "keyring-macos-check: OK",
  );
  if (failures.length) process.exitCode = 1;
}

if (process.argv[2] === INNER) await inner(process.argv[3]);
else await outer();
