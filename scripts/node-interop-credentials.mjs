// rust-cli 与 Node 读写 `enc:v2` 凭据（docs/specs/rust-cli/platform.md §13，CRED-V2）：node-interop-check.mjs 的第三十七段。
// 全部是临时 HOME 与本地假服务；系统凭据库换成一个 JSON 文件（Node 一侧注入 `createSystemCredentialCipher` 的 `keyStore`，
// rust-cli 一侧是 debug 构建的 `_ZCODE_ASSUME_KEYRING_FILE`），不碰真实的钥匙串、凭据管理器与 Secret Service。
// 不给 ZCODE_CREDENTIAL_SECRET：两边都走系统凭据库（来源 os）。受管密钥（来源 env）两边互读在第十四、十五段与
// login-interop-check.mjs。
//
// A. App 用系统密钥登录（`enc:v2:os`）：rust-cli 的 `zcode -p` 读出 App 的登录、用个人套餐，写的项目位置也是 v2:os；
//    App 的请求期换取复用它（解得开 rust-cli 写的值）。
// B. App 的旧登录（`enc:v1`，Node 的缺省派生密钥）：rust-cli 不迁移（用户决定 2026-11-11），照 v1 读写、不碰凭据库，
//    照常回答，写的项目位置也是 v1；App 换成系统密钥之后读时迁移整个文件，读出同一个账号、复用 rust-cli 写的位置。
// C. 空 home 里 rust-cli 先写（手工 key 登录）：rust-cli 建密钥，Node 独立 CLI 的凭据仓库（同一个假凭据库）读出这把 key。
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import {
  appAccount,
  builtinConfig,
  createService,
  credentialsFile,
  listen,
  run,
} from "./node-interop-app-login.mjs";
import { importNode } from "./node-root.mjs";

/** Node `SYSTEM_SERVICE`（systemCredentialCipher.ts）。 */
const SERVICE = "ZCode Credential Encryption";
const LOCATION = "project-key-location:v1:";
const KEY_LIST = "/api/biz/v1/organization/org-1/projects/p-1/api_keys";

/** Node `systemKeyStore` 的账号名：sha256(path.resolve(凭据文件)) 的 hex。 */
const accountOf = (file) => createHash("sha256").update(resolve(file)).digest("hex");

/** 文件里的假凭据库：与 rust-cli 的 `FileKeyStore` 同一种格式 `{ "<服务名>": { "<账号名>": "<值>" } }`。 */
function fileKeyStore(keyring, credentials) {
  const account = accountOf(credentials);
  const entries = async () => {
    try {
      return JSON.parse(await readFile(keyring, "utf8"));
    } catch (error) {
      if (error.code === "ENOENT") return {};
      throw error;
    }
  };
  return {
    get: async () => (await entries())[SERVICE]?.[account] ?? null,
    set: async (value) => {
      const all = await entries();
      all[SERVICE] = { ...all[SERVICE], [account]: value };
      await writeFile(keyring, JSON.stringify(all), { mode: 0o600 });
    },
  };
}

export async function checkCredentials({ nodeRoot, repoRoot, base, expectEqual, zcode }) {
  const service = createService();
  const { server, url } = await listen(service);
  const findings = {};
  const saved = { bigmodel: process.env.BIGMODEL_API_BASE_URL, home: process.env.HOME };
  // App 的 bigmodel 地址在调用时读 process.env（同第十五段）。
  process.env.BIGMODEL_API_BASE_URL = url;
  const app = await appAccount({ nodeRoot, service, url });
  try {
    const homeEnv = async (name) => {
      const home = join(base, name);
      await mkdir(join(home, "work/.git"), { recursive: true });
      const keyring = join(home, "keyring.json");
      return {
        home,
        keyring,
        // 每次一个新的 keyStore 对象：Node 按 keyStore 实例缓存密钥，相当于一个新进程。
        osCipher: () =>
          app.createSystemCredentialCipher({
            filePath: credentialsFile(home),
            env: {},
            keyStore: fileKeyStore(keyring, credentialsFile(home)),
          }),
        env: {
          HOME: home,
          USERPROFILE: home,
          PATH: process.env.PATH,
          ZCODE_DATA_BASE_DIR: home,
          _ZCODE_ASSUME_KEYRING_FILE: keyring,
          ZCODE_BASE_URL: url,
          ZAI_BUSINESS_BASE_URL: url,
          BIGMODEL_API_BASE_URL: url,
          ZCODE_BUILTIN_PROVIDER_CONFIG_FILE: await builtinConfig(
            repoRoot,
            url,
            join(home, "builtin.json"),
          ),
        },
      };
    };
    const record = async (home) => JSON.parse(await readFile(credentialsFile(home), "utf8"));
    const keyringEntries = async (keyring) =>
      Object.values(JSON.parse(await readFile(keyring, "utf8"))[SERVICE] ?? {});
    const since = (mark, who) => service.seen.slice(mark).filter((r) => r.who === who);
    const print = async (home, env) => {
      const mark = service.seen.length;
      const result = await run(zcode, ["-p", "hello"], env, join(home, "work"));
      return { result, requests: since(mark, "rust") };
    };
    const allOs = (values) => values.every((value) => value.startsWith("enc:v2:os:"));

    // A. App 用系统密钥登录；rust-cli 读、写；App 复用 rust-cli 写的位置。
    {
      const { home, env, keyring, osCipher } = await homeEnv("os-a");
      await app.login(home, osCipher());
      expectEqual(
        "creds A: the App login is enc:v2:os",
        true,
        allOs(Object.values(await record(home))),
      );
      const rust = await print(home, env);
      expectEqual(
        "creds A: rust-cli answers with the App login (system key)",
        "done",
        rust.result.stdout.trim() || rust.result.stderr,
      );
      const model = rust.requests.find((r) => r.path.endsWith("/v1/messages"));
      expectEqual(
        "creds A: the model request carries a project token",
        true,
        /^Bearer pat-\d+$/.test(model?.authorization ?? ""),
      );
      const after = await record(home);
      const locations = Object.keys(after).filter((key) => key.startsWith(LOCATION));
      expectEqual("creds A: rust-cli wrote one project location", 1, locations.length);
      expectEqual("creds A: rust-cli writes enc:v2:os", true, allOs(Object.values(after)));
      const mark = service.seen.length;
      const individual = await app.individual(home, osCipher());
      findings.appReadsRustV2 = Boolean(individual.material?.token);
      expectEqual(
        "creds A: the App reads the location rust-cli wrote",
        true,
        findings.appReadsRustV2,
      );
      expectEqual(
        "creds A: the App reuses it (no key list)",
        false,
        since(mark, "app").some((r) => r.path === KEY_LIST),
      );
      expectEqual(
        "creds A: one key in the system store",
        1,
        (await keyringEntries(keyring)).length,
      );
    }

    // B. App 的旧登录（v1）：rust-cli 照 v1 读写、不碰凭据库；App 换成系统密钥后读时迁移，读得出 rust-cli 写的 v1 值。
    {
      const { home, env, keyring, osCipher } = await homeEnv("os-b");
      // Node 的 v1 缺省派生按 `os.homedir()`（读 HOME）：登录期间让本进程的 HOME 是这个 home，与 rust-cli 的 HOME 相同。
      process.env.HOME = home;
      try {
        await app.login(home, app.createLegacyCredentialCipher({}));
      } finally {
        process.env.HOME = saved.home;
      }
      const allV1 = (values) => values.every((v) => v.startsWith("enc:v1:"));
      expectEqual(
        "creds B: the old App login is enc:v1",
        true,
        allV1(Object.values(await record(home))),
      );
      const rust = await print(home, env);
      expectEqual(
        "creds B: rust-cli answers with the old login",
        "done",
        rust.result.stdout.trim() || rust.result.stderr,
      );
      const after = await record(home);
      const locations = Object.keys(after).filter((key) => key.startsWith(LOCATION));
      expectEqual("creds B: rust-cli wrote one project location", 1, locations.length);
      findings.rustKeepsV1 = allV1(Object.values(after));
      expectEqual(
        "creds B: every value is still enc:v1 (no migration)",
        true,
        findings.rustKeepsV1,
      );
      expectEqual("creds B: rust-cli did not touch the system store", false, existsSync(keyring));
      expectEqual(
        "creds B: rust-cli pinned no source",
        false,
        existsSync(join(home, ".zcode/v2/credential-source")),
      );
      const mark = service.seen.length;
      // App 解 v1 的缺省派生同样按本进程的 HOME（真实的 App 与 rust-cli 是同一个用户、同一个 HOME）
      process.env.HOME = home;
      let individual;
      try {
        individual = await app.individual(home, osCipher());
      } finally {
        process.env.HOME = saved.home;
      }
      expectEqual(
        "creds B: the App gets a project token",
        true,
        Boolean(individual.material?.token),
      );
      expectEqual(
        "creds B: the App reuses the v1 location rust-cli wrote (no key list)",
        false,
        since(mark, "app").some((r) => r.path === KEY_LIST),
      );
      expectEqual(
        "creds B: the App migrated the file to enc:v2:os",
        true,
        allOs(Object.values(await record(home))),
      );
    }

    // C. 空 home 里 rust-cli 先写：rust-cli 建密钥，Node 独立 CLI 读出 key。
    {
      const { home, env, keyring, osCipher } = await homeEnv("os-c");
      const login = await run(
        zcode,
        ["login", "bigmodel-coding-plan-api-key"],
        env,
        home,
        "sk-rust-os\n",
      );
      expectEqual("creds C: rust-cli manual login", 0, login.code === 0 ? 0 : login.stderr);
      expectEqual("creds C: rust-cli created one key", 1, (await keyringEntries(keyring)).length);
      const { createSharedZCodeCredentialStore } = await importNode(
        nodeRoot,
        "adapters/src/auth/shared-credentials.ts",
      );
      const store = createSharedZCodeCredentialStore({
        filePath: credentialsFile(home),
        cipher: osCipher(),
      });
      const provider = "account:bigmodel-individual-coding-plan";
      const identity = await store.load(`account-provider:${provider}:identity`);
      const key = await store.load(
        `account-provider:coding-plan:${provider}:account:${encodeURIComponent(identity ?? "")}:api-key`,
      );
      findings.nodeReadsRustKey = key === "sk-rust-os";
      expectEqual("creds C: Node reads the key rust-cli wrote", "sk-rust-os", key);
    }
  } finally {
    app.reset();
    process.env.HOME = saved.home;
    if (saved.bigmodel === undefined) delete process.env.BIGMODEL_API_BASE_URL;
    else process.env.BIGMODEL_API_BASE_URL = saved.bigmodel;
    await new Promise((done) => server.close(done));
  }
  return findings;
}
