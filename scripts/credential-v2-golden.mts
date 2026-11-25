#!/usr/bin/env node
// rust-cli 读写 `enc:v2` 凭据的对照夹具（docs/specs/rust-cli/platform.md §13.10，CRED-V2）：用 Node 的
// `createSystemCredentialCipher` 与 `createLegacyServerCredentialCipher` 生成，本工程的测试逐项比较。不碰真实的凭据库：
// 系统凭据库换成注入的固定 keyStore，凭据文件在临时目录；受管密钥是测试值。
//
//   node_modules/.bin/tsx scripts/rust-cli/credential-v2-golden.mts \
//     packages/rust-cli/crates/platform/tests/fixtures/credential-v2-golden.json [--node-root <装好依赖的检出>]
//
// 四部分：
// - values：同一组明文在 `os` 来源（固定的系统密钥）与 `env` 来源（带首尾空白的受管密钥，v2 不去空白）下的密文；
// - legacy：受管密钥原值与去空白之后各自的 `enc:v1` 密文（Node 的 v1 候选密钥两种都试）；
// - legacyWrite：一份只有 v1 与明文的文件，Node 的 `createLegacyServerCredentialCipher`（v1 文件的写密钥规则）写的新值；
// - accounts：凭据文件路径到系统凭据库账号名的算法（sha256(path.resolve(file)) 的 hex）。
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const args = process.argv.slice(2);
const rootFlag = args.indexOf("--node-root");
const nodeRoot =
  rootFlag >= 0 ? resolve(args.splice(rootFlag, 2)[1]!) : resolve(import.meta.dirname, "../..");
const [outFile] = args;
if (!outFile) throw new Error("usage: credential-v2-golden.mts <out.json> [--node-root <dir>]");

const load = (path: string) => import(pathToFileURL(join(nodeRoot, path)).href);
const {
  createSystemCredentialCipher,
  createLegacyCredentialCipher,
  createLegacyServerCredentialCipher,
} = await load("packages/shared/src/credential-security/systemCredentialCipher.ts");

// 系统密钥：32 字节的固定值（测试值），base64url 无填充 43 个字符。
const OS_KEY = Buffer.from(Array.from({ length: 32 }, (_, index) => index + 1)).toString(
  "base64url",
);
const ENV_SECRET = " golden secret ";
const PLAINS = ["sk-golden-国", "", JSON.stringify({ user_id: "u-1", name: "Dev" })];

const home = mkdtempSync(join(tmpdir(), "rust-cli-credential-v2-golden-"));
try {
  const file = join(home, ".zcode/v2/credentials.json");
  const fixedStore = {
    get: async () => OS_KEY,
    set: async () => {
      throw new Error("the golden key store is read-only");
    },
  };
  const os = createSystemCredentialCipher({ filePath: file, env: {}, keyStore: fixedStore });
  const env = createSystemCredentialCipher({
    filePath: file,
    env: { ZCODE_CREDENTIAL_SECRET: ENV_SECRET },
  });
  const values = [];
  for (const plain of PLAINS) {
    values.push({ plain, os: await os.encrypt(plain), env: await env.encrypt(plain) });
  }
  const legacy = {
    raw: createLegacyCredentialCipher({ ZCODE_CREDENTIAL_SECRET: ENV_SECRET }).encrypt("raw-国"),
    trimmed: createLegacyCredentialCipher({ ZCODE_CREDENTIAL_SECRET: ENV_SECRET.trim() }).encrypt(
      "trimmed",
    ),
  };
  // v1 文件的写密钥：旧值是去空白的 secret 写的，另有一个明文；Node 选能解开旧值的那个候选密钥写新值。
  const legacyFile = join(home, "legacy/.zcode/v2/credentials.json");
  const legacyRecord = { old: legacy.trimmed, plain: "plain-token" };
  mkdirSync(dirname(legacyFile), { recursive: true });
  writeFileSync(legacyFile, JSON.stringify(legacyRecord));
  const legacyServer = await createLegacyServerCredentialCipher({
    filePath: legacyFile,
    env: { ZCODE_CREDENTIAL_SECRET: ENV_SECRET },
  });
  const legacyWrite = {
    file: legacyRecord,
    plain: "legacy-write",
    sealed: legacyServer.encrypt("legacy-write"),
  };
  // Node 的算法（`systemKeyStore`）：sha256(path.resolve(file)) 的 hex。路径是示例值。
  const accounts = [
    "/Users/dev/.zcode/v2/credentials.json",
    "/home/dev/./x/../.zcode/v2/credentials.json",
  ].map((path) => ({ path, account: createHash("sha256").update(resolve(path)).digest("hex") }));
  const golden = {
    generator: "scripts/rust-cli/credential-v2-golden.mts",
    service: "ZCode Credential Encryption",
    osKey: OS_KEY,
    envSecret: ENV_SECRET,
    values,
    legacy: { ...legacy, rawPlain: "raw-国", trimmedPlain: "trimmed" },
    legacyWrite,
    accounts,
  };
  writeFileSync(outFile, `${JSON.stringify(golden, null, 2)}\n`);
} finally {
  rmSync(home, { recursive: true, force: true });
}
