// rust-cli 读 Node 写的插件配置值与敏感值（PLUG-P0，docs/specs/rust-cli/ext.md §4.9；node-interop-check.mjs 的第三十八段）。
//
// Node 的 `savePluginOptions` 按它的规则写：普通值进配置文件的 `plugins.options`，`sensitive` 的键进 `plugin-secrets.json`（用户级进
// `user`，项目层进 `workspaces[<规范化的根>]`）。之后：
// - Node：`createConfig` 加 `withPluginSecrets` 算出这个插件的配置值（项目层高于用户级，同一层里私有文件优先）；
// - rust-cli：真实的 `zcode -p --plugin-dir <插件>` 里插件的 hook 把 `$ZCODE_PLUGIN_OPTION_<KEY>` 写进文件。
// 两边的值相同。
import { spawn } from "node:child_process";
import { realpathSync } from "node:fs";
import { mkdir, readFile, writeFile } from "node:fs/promises";
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

/** `--plugin-dir` 的插件 id（`<name>@inline`）。 */
const PLUGIN_ID = "kit@inline";
const USER_CONFIG = {
  color: { type: "string", title: "Color", description: "A plain option" },
  token: { type: "string", title: "Token", description: "A sensitive option", sensitive: true },
  size: { type: "number", title: "Size", description: "A plain number" },
};
/** hook 写出的文件：三个配置值的环境变量，用 `|` 连。 */
const REPORT = "options.txt";

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

export async function checkPluginOptions({ nodeRoot, base, expectEqual, zcode }) {
  const { createConfig } = await importNode(nodeRoot, "adapters/src/config/config-factory.ts");
  const { savePluginOptions, withPluginSecrets, resolvePluginSecretsPath } = await importNode(
    nodeRoot,
    "bootstrap/src/plugin-secrets.ts",
  );
  const home = join(base, "home");
  await mkdir(join(home, ".zcode", "cli"), { recursive: true });
  await mkdir(join(base, "work", ".git"), { recursive: true });
  const work = realpathSync(join(base, "work"));
  await mkdir(join(work, ".zcode"), { recursive: true });
  const kit = join(base, "kit");
  await mkdir(join(kit, ".zcode-plugin"), { recursive: true });
  await mkdir(join(kit, "hooks"), { recursive: true });
  await writeFile(
    join(kit, ".zcode-plugin", "plugin.json"),
    `${JSON.stringify({ name: "kit", userConfig: USER_CONFIG }, null, 2)}\n`,
  );
  const script = `printf '%s|%s|%s' "$ZCODE_PLUGIN_OPTION_COLOR" "$ZCODE_PLUGIN_OPTION_TOKEN" "$ZCODE_PLUGIN_OPTION_SIZE" > "$ZCODE_PROJECT_DIR/${REPORT}"`;
  await writeFile(
    join(kit, "hooks", "hooks.json"),
    `${JSON.stringify({ hooks: { UserPromptSubmit: [{ hooks: [{ type: "command", command: script }] }] } })}\n`,
  );
  const secretsPath = resolvePluginSecretsPath(join(home, ".zcode", "cli", "plugins"));
  const userConfig = join(home, ".zcode", "cli", "config.json");
  const projectConfig = join(work, ".zcode", "config.json");
  const plugin = { id: PLUGIN_ID, userConfig: USER_CONFIG };
  const env = { HOME: home, PATH: process.env.PATH, ZCODE_DATA_BASE_DIR: home };
  const node = await withHome(home, async () => {
    // 用户级：普通值进 config.json，敏感值进私有文件的 `user`。
    await savePluginOptions({
      clearOptionKeys: [],
      configPath: userConfig,
      options: { color: "blue", token: "user-token", size: 3 },
      plugin,
      scope: { kind: "user" },
      secretsPath,
    });
    // 项目层：普通值进项目配置，敏感值进私有文件的 `workspaces[<根>]`。
    await savePluginOptions({
      clearOptionKeys: [],
      configPath: projectConfig,
      options: { color: "red", token: "workspace-token" },
      plugin,
      scope: { kind: "workspace", root: work },
      secretsPath,
    });
    const { configResult } = await withPluginSecrets(
      createConfig({ env, workingDirectory: work }),
      secretsPath,
    );
    return configResult.config.plugins?.options?.[PLUGIN_ID];
  });
  expectEqual("Node resolves the plugin options", node, {
    color: "red",
    token: "workspace-token",
    size: 3,
  });
  const secrets = JSON.parse(await readFile(secretsPath, "utf8"));
  expectEqual(
    "Node keeps the sensitive values out of the config files",
    [secrets.user?.[PLUGIN_ID]?.token, Object.keys(secrets.workspaces ?? {}).length],
    ["user-token", 1],
  );

  const model = await modelServer();
  try {
    const code = await new Promise((resolve, reject) => {
      const child = spawn(zcode, ["-p", "plugin options probe", "--plugin-dir", kit], {
        cwd: work,
        env: {
          ...env,
          ZCODE_MODEL_BASE_URL: model.url,
          ZCODE_API_KEY: "test-key",
          ZCODE_MODEL: GLM_5_3,
        },
        stdio: ["ignore", "ignore", "inherit"],
      });
      child.on("error", reject);
      child.on("exit", resolve);
    });
    expectEqual("zcode -p with the plugin exits 0", code, 0);
  } finally {
    model.server.close();
  }
  const reported = await readFile(join(work, REPORT), "utf8").catch(() => "");
  expectEqual(
    "rust-cli gives the plugin the options Node resolved",
    reported,
    `${node?.color}|${node?.token}|${node?.size}`,
  );
}
