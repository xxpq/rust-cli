#!/usr/bin/env node
// Linux Secret Service 的手动核对（docs/specs/rust-cli/platform.md §13.10，CRED-V2）：在一次性的 Docker 容器里，rust 的
// `zcode-rs`（aarch64 / x86_64 的 musl 产物）与 Node 的凭据仓库（真实的 `createSystemCredentialCipher`，系统凭据库是
// `@napi-rs/keyring` 的 Secret Service）读写同一个凭据文件与同一个 Secret Service 条目。不进自动测试，不碰本机的任何凭据库。
//
//   node scripts/rust-cli/build-linux.mjs --debug --target aarch64-unknown-linux-musl   # 先编产物（容器的架构）
//   node scripts/rust-cli/keyring-linux-check.mjs [--image node:24-bookworm]
//
// 容器里装 dbus 与 gnome-keyring，用测试口令解锁一个新的 login 钥匙环；仓库只读挂进去，HOME 是容器里的临时目录。
// 1. 没有会话总线：新文件两边都报凭据库不可用（Node 的原文）；给了 ZCODE_CREDENTIAL_SECRET 时 rust 写 `enc:v2:env`、Node 读得出；
//    只有 v1 的文件（Node 的缺省派生写的）rust 照 v1 写、不要凭据库（用户决定 2026-11-11），Node 的 v1 读法读得出；
// 2. 有 Secret Service、rust 先写（建密钥）：Node 读出 rust 写的 key；
// 3. Node 先写（建密钥）：rust 用 Node 建的密钥写，Node 读出两边的值；两个文件里都是 `enc:v2:os`。
import { execFile, spawn } from "node:child_process";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";

const run = promisify(execFile);
const repoRoot = resolve(import.meta.dirname, "../..");
const INNER = "--inner";
const UNAVAILABLE = "System credential storage is unavailable.";
const PROVIDER = "account:bigmodel-individual-coding-plan";

/** 宿主：起容器，把整个核对交给容器里的同一个脚本。 */
async function outer(argv) {
  const imageFlag = argv.indexOf("--image");
  const image = imageFlag >= 0 ? argv[imageFlag + 1] : "node:24-bookworm";
  const { stdout: arch } = await run("docker", ["info", "--format", "{{.Architecture}}"]);
  const triple =
    arch.trim() === "aarch64" ? "aarch64-unknown-linux-musl" : "x86_64-unknown-linux-musl";
  const binary = join(repoRoot, "packages/rust-cli/target", triple, "debug/zcode-rs");
  await readFile(binary).catch(() => {
    throw new Error(
      `build ${binary} first: node scripts/rust-cli/build-linux.mjs --debug --target ${triple}`,
    );
  });
  const work = await mkdtemp(join(tmpdir(), "rust-cli-secret-service-"));
  const inner = [
    "set -e",
    // gnome-keyring 的控制套接字放在 $HOME/.cache（没有 XDG_RUNTIME_DIR 时）
    'mkdir -p "$HOME/.cache"',
    "apt-get update -qq >/dev/null",
    "DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends dbus gnome-keyring >/dev/null",
    `node --import ${repoRoot}/node_modules/tsx/dist/loader.mjs ${import.meta.filename} ${INNER} nobus ${binary}`,
    // 会话总线里起 gnome-keyring，测试口令解锁（新建）login 钥匙环
    `dbus-run-session -- sh -c 'printf test | gnome-keyring-daemon --unlock --components=secrets --daemonize >/dev/null && node --import ${repoRoot}/node_modules/tsx/dist/loader.mjs ${import.meta.filename} ${INNER} bus ${binary}'`,
  ].join(" && ");
  try {
    const child = spawn(
      "docker",
      [
        "run",
        "--rm",
        "-v",
        `${repoRoot}:${repoRoot}:ro`,
        "-v",
        `${work}:/work`,
        "-e",
        "HOME=/work/home",
        image,
        "sh",
        "-c",
        inner,
      ],
      { stdio: "inherit" },
    );
    process.exitCode = await new Promise((done) => child.on("close", done));
  } finally {
    await rm(work, { recursive: true, force: true });
  }
}

/** 容器里：`mode` 是 nobus（没有会话总线）或 bus（有 Secret Service）。 */
async function inner(mode, binary) {
  const failures = [];
  const expect = (label, ok, detail = "") => {
    console.log(`${ok ? "ok  " : "FAIL"} [${mode}] ${label}${ok || !detail ? "" : `: ${detail}`}`);
    if (!ok) failures.push(label);
  };
  const { createLegacyCredentialCipher } = await import(
    pathToFileURL(
      join(repoRoot, "packages/shared/src/credential-security/systemCredentialCipher.ts"),
    ).href
  );
  const { createSharedZCodeCredentialStore } = await import(
    pathToFileURL(join(repoRoot, "apps/zcode-cli/packages/adapters/src/auth/shared-credentials.ts"))
      .href
  );
  const homeOf = async (name) => {
    const home = join("/work", mode, name);
    await mkdir(home, { recursive: true });
    return home;
  };
  const file = (home) => join(home, ".zcode/v2/credentials.json");
  const env = (home, extra = {}) => {
    const base = { PATH: process.env.PATH, HOME: home, ZCODE_DATA_BASE_DIR: home, ...extra };
    for (const name of ["DBUS_SESSION_BUS_ADDRESS", "XDG_RUNTIME_DIR"]) {
      if (process.env[name]) base[name] = process.env[name];
    }
    return base;
  };
  const rustLogin = (home, key, extra) =>
    new Promise((done) => {
      const child = spawn(binary, ["login", "bigmodel-coding-plan-api-key"], {
        env: env(home, extra),
      });
      let stderr = "";
      child.stderr.on("data", (chunk) => (stderr += chunk));
      child.on("close", (code) => done({ code, stderr }));
      child.stdin.end(`${key}\n`);
    });
  const nodeStore = (home, extra = {}) =>
    createSharedZCodeCredentialStore({ filePath: file(home), env: { ...env(home), ...extra } });
  const nodeKey = async (store) => {
    const identity = await store.load(`account-provider:${PROVIDER}:identity`);
    return store.load(
      `account-provider:coding-plan:${PROVIDER}:account:${encodeURIComponent(identity ?? "")}:api-key`,
    );
  };
  const values = async (home) => Object.values(JSON.parse(await readFile(file(home), "utf8")));

  if (mode === "nobus") {
    const home = await homeOf("unavailable");
    const login = await rustLogin(home, "sk-rust");
    expect(
      "rust reports the store as unavailable",
      login.code !== 0 && login.stderr.includes(UNAVAILABLE),
      login.stderr,
    );
    const saved = await nodeStore(home)
      .save("interop:node", "x")
      .then(
        () => "saved",
        (error) => error.code,
      );
    expect(
      "Node reports the store as unavailable",
      saved === "CREDENTIAL_SECURE_STORAGE_UNAVAILABLE",
      saved,
    );
    const managed = await homeOf("managed");
    const secret = { ZCODE_CREDENTIAL_SECRET: "linux-check-secret" };
    const withSecret = await rustLogin(managed, "sk-rust-env", secret);
    expect("rust writes with a managed secret", withSecret.code === 0, withSecret.stderr);
    expect(
      "the values are enc:v2:env",
      (await values(managed)).every((v) => v.startsWith("enc:v2:env:")),
    );
    expect("Node reads the key", (await nodeKey(nodeStore(managed, secret))) === "sk-rust-env");
    // v1 文件：Node 的缺省派生按 `os.homedir()`（读 HOME），写的时候让本进程的 HOME 与 rust 的相同
    const legacyHome = await homeOf("v1");
    const legacyStore = () => {
      process.env.HOME = legacyHome;
      return createLegacyCredentialCipher({});
    };
    await mkdir(dirname(file(legacyHome)), { recursive: true });
    await writeFile(
      file(legacyHome),
      JSON.stringify({ "interop:old": legacyStore().encrypt("old") }),
    );
    const v1 = await rustLogin(legacyHome, "sk-rust-v1");
    expect("rust writes a v1 file without Secret Service", v1.code === 0, v1.stderr);
    const written = JSON.parse(await readFile(file(legacyHome), "utf8"));
    expect(
      "the values stay enc:v1",
      Object.values(written).every((v) => v.startsWith("enc:v1:")),
    );
    const identity = legacyStore().decrypt(written[`account-provider:${PROVIDER}:identity`]);
    const sealedKey =
      written[
        `account-provider:coding-plan:${PROVIDER}:account:${encodeURIComponent(identity)}:api-key`
      ];
    expect("Node's v1 reader reads the key", legacyStore().decrypt(sealedKey) === "sk-rust-v1");
  } else {
    const rustFirst = await homeOf("rust-first");
    const login = await rustLogin(rustFirst, "sk-rust-os");
    expect("rust creates the key in Secret Service", login.code === 0, login.stderr);
    expect("Node reads the key rust wrote", (await nodeKey(nodeStore(rustFirst))) === "sk-rust-os");
    const nodeFirst = await homeOf("node-first");
    await nodeStore(nodeFirst).save("interop:node", "kept");
    const second = await rustLogin(nodeFirst, "sk-rust-second");
    expect("rust writes with the key Node created", second.code === 0, second.stderr);
    const store = nodeStore(nodeFirst);
    expect("Node reads the key rust wrote", (await nodeKey(store)) === "sk-rust-second");
    expect("Node still reads its own value", (await store.load("interop:node")) === "kept");
    for (const home of [rustFirst, nodeFirst]) {
      expect(
        `the values are enc:v2:os (${home})`,
        (await values(home)).every((v) => v.startsWith("enc:v2:os:")),
      );
    }
  }
  if (failures.length) process.exitCode = 1;
}

if (process.argv[2] === INNER) await inner(process.argv[3], process.argv[4]);
else await outer(process.argv.slice(2));
