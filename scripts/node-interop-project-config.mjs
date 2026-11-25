// rust-cli 与 Node CLI 读同一份项目配置（PROJECT-CONFIG，docs/specs/rust-cli/config.md §1.1；node-interop-check.mjs 的第三十一段）。
//
// 仓库的 `zcode.json` 写想放权、想改存储的键（`permission.mode: yolo`、`allowedTools: ["Bash(*)"]`、`storage.sessionDbPath`、
// `storage.dir`），另写一个扩展声明（`plugins.enabledPlugins`）。两边都只读扩展声明：
// - Node：`createConfig`（项目文件经 `projectConfigFileSchema` 的白名单）没有项目的权限模式与存储位置，`enabledPlugins` 照常；
// - rust-cli：真实的 `zcode -p` 把会话写进缺省的库（不是项目指定的库，也不建项目指定的存储根），会话的模式是 Node 的 build。
// Node 按自己的解析找到同一个库，读到这次的会话与它的模式。
import { spawn } from "node:child_process";
import { existsSync, realpathSync } from "node:fs";
import { mkdir, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { GLM_5_3 } from "./fixture/model.mjs";
import { importNode, withHome } from "./node-root.mjs";

const REPLY = [
  'event: message_start\ndata: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"m","content":[],"stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1}}}\n\n',
  'event: content_block_start\ndata: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}\n\n',
  'event: content_block_delta\ndata: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"done"}}\n\n',
  'event: content_block_stop\ndata: {"type":"content_block_stop","index":0}\n\n',
  'event: message_delta\ndata: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}\n\n',
  'event: message_stop\ndata: {"type":"message_stop"}\n\n',
].join("");

/** Node 的缺省权限模式名（`permission.mode` 没有时新会话用它）。 */
const NODE_DEFAULT_MODE = "build";
/** 项目文件里的扩展声明：两边都读。 */
const PLUGIN_ID = "kit@example-marketplace";

/** 本地模型：每个请求回同一段文本回复。 */
async function modelServer() {
  const server = createServer((request, response) => {
    request.resume();
    request.on("end", () => {
      response.writeHead(200, { "content-type": "text/event-stream" });
      response.end(REPLY);
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return { server, url: `http://127.0.0.1:${server.address().port}` };
}

export async function checkProjectConfig({ nodeRoot, base, expectEqual, zcode }) {
  const { createConfig } = await importNode(nodeRoot, "adapters/src/config/config-factory.ts");
  const { getSessionDbPath } = await importNode(nodeRoot, "bootstrap/src/app/session-store.ts");
  const { SqliteSessionStore } = await importNode(
    nodeRoot,
    "adapters/src/storage/session-store/sqlite-session-store.ts",
  );
  const home = join(base, "home");
  await mkdir(home, { recursive: true });
  await mkdir(join(base, "work", ".git"), { recursive: true });
  // 会话行的目录是进程的 cwd（macOS 的临时目录经符号链接解析成 /private/…）。
  const work = realpathSync(join(base, "work"));
  const projectDb = join(base, "project.sqlite");
  const projectStore = join(base, "project-store");
  await writeFile(
    join(work, "zcode.json"),
    `${JSON.stringify(
      {
        permission: { mode: "yolo", allowedTools: ["Bash(*)"] },
        storage: { sessionDbPath: projectDb, dir: projectStore },
        plugins: { enabledPlugins: { [PLUGIN_ID]: false } },
      },
      null,
      2,
    )}\n`,
  );
  const env = { HOME: home, PATH: process.env.PATH, ZCODE_DATA_BASE_DIR: home };
  // 对照：同一个 home 下没有项目文件的仓库。Node 的缺省值（模式 build、缺省的存储根）照它算，项目文件不该改变它们。
  await mkdir(join(base, "bare", ".git"), { recursive: true });
  const bare = realpathSync(join(base, "bare"));
  // `createConfig` 读用户配置、`getSessionDbPath` 展开 `~` 都按本进程的 home：两者都在 `withHome` 里调，不碰本机的 ~/.zcode。
  const load = (workingDirectory) =>
    withHome(home, () => {
      const result = createConfig({ env, workingDirectory });
      return { config: result.config, dbPath: getSessionDbPath(result) };
    });
  const withProject = await load(work);
  const without = await load(bare);
  expectEqual(
    "Node does not take the project permission settings",
    withProject.config.permission,
    without.config.permission,
  );
  expectEqual(
    "Node does not take the project storage settings",
    withProject.config.storage,
    without.config.storage,
  );
  expectEqual(
    "Node keeps the project enabledPlugins",
    withProject.config.plugins?.enabledPlugins?.[PLUGIN_ID],
    false,
  );
  const dbPath = join(home, ".zcode", "cli", "db", "db.sqlite");
  expectEqual("Node resolves the default session db", withProject.dbPath, dbPath);
  expectEqual("Node's default mode is build", without.config.permission?.mode, NODE_DEFAULT_MODE);

  const model = await modelServer();
  try {
    const code = await new Promise((resolve, reject) => {
      const child = spawn(zcode, ["-p", "project config probe"], {
        cwd: work,
        env: {
          ...env,
          // rust-cli 只读 ZCODE_* 的名字（修复线 F42，config.md §2.5）。
          ZCODE_MODEL_BASE_URL: model.url,
          ZCODE_API_KEY: "test-key",
          ZCODE_MODEL: GLM_5_3,
        },
        stdio: ["ignore", "ignore", "inherit"],
      });
      child.on("error", reject);
      child.on("exit", resolve);
    });
    expectEqual("zcode -p under the project config exits 0", code, 0);
  } finally {
    model.server.close();
  }
  expectEqual("rust-cli did not use the project session db", existsSync(projectDb), false);
  expectEqual("rust-cli did not create the project storage dir", existsSync(projectStore), false);
  expectEqual("rust-cli wrote the session to the db Node resolves", existsSync(dbPath), true);
  if (!existsSync(dbPath)) return;
  const store = await SqliteSessionStore.openStartup({ dbPath });
  try {
    const listed = await store.listSessions({ directory: work, roots: true });
    expectEqual("Node lists the zcode -p session", listed.length, 1);
    if (listed.length !== 1) return;
    const session = await store.getSession(listed[0].id);
    expectEqual(
      "Node reads the default mode, not the project's yolo",
      session.permission?.mode,
      NODE_DEFAULT_MODE,
    );
  } finally {
    store.close();
  }
}
