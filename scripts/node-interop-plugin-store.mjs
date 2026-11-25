// rust-cli 与 Node CLI 共写插件存储（PLUG-P2，docs/specs/rust-cli/plugin-store.md §14、§16；node-interop-check.mjs 的第三十九段）。
//
// 用真实的 `zcode-rs plugin …` 与 Node 的真实插件代码（bootstrap 的 `pluginMutation` 入口，持同一把存储锁），只用临时 HOME，
// 本地目录市场与本机 HTTP 服务给的 zip，不联网：
// 1. rust 装的，Node 能加载、能卸载（Node 按 `installPath` 删 rust 的不可变目录）；
// 2. Node 装的，rust 能加载、能更新（新的不可变目录；Node 的旧目录打孤儿标记，不删）；
// 3. 两个进程并发安装不丢记录（三轮，各装不同的插件）；
// 4. Node 持存储锁下载时被杀：rust 回收锁、照样装；
// 5. rust 在锁外暂存时被杀、或持锁时崩溃（锁目录里留下已死的 owner，同形）：Node 照样装；rust 下次写这个插件目录时删掉死掉的暂存；
// 6. Node 提交之后、收尾之前崩溃（市场目录的事务文件与备份留着）：rust 刷新这个市场时先按 Node 的规则收尾，Node 之后读到的是
//    rust 写的新清单，不会回滚到备份（没有收尾时 Node 会判成「没提交」，把旧清单换回来）。
import { execFile, spawn } from "node:child_process";
import { once } from "node:events";
import { realpathSync } from "node:fs";
import { mkdir, mkdtemp, readdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { isDeepStrictEqual, promisify } from "node:util";
import { refuseRealHome } from "./node-interop-home.mjs";
import { buildRuntimeBinaries } from "./node-interop-runtime.mjs";
import { ensureTypeScript, importNode, nodeRootFromArgs, withHome } from "./node-root.mjs";

/** Node 的列出不播种内置插件（会把仓库里的官方插件拷进临时 HOME）：给一个什么都不做的准备。 */
const NO_SEED = {
  signal: new AbortController().signal,
  prepare: async () => [],
  resolvePlugins: (resolve) => resolve(),
  invalidate: () => {},
};
/** 一个不可能活着的 pid（rust 崩溃时留下的锁的 owner）。 */
const DEAD_PID = 2147483646;

const json = (value) => `${JSON.stringify(value, null, 2)}\n`;
const exists = (path) =>
  stat(path).then(
    () => true,
    () => false,
  );

async function writePlugin(market, name, version) {
  const root = join(market, name);
  await mkdir(join(root, ".claude-plugin"), { recursive: true });
  await mkdir(join(root, "skills", name), { recursive: true });
  await writeFile(join(root, ".claude-plugin", "plugin.json"), json({ name, version }));
  await writeFile(
    join(root, "skills", name, "SKILL.md"),
    `---\ndescription: ${name} ${version}\n---\n${name}\n`,
  );
}

async function writeCatalog(market, name, plugins) {
  await mkdir(join(market, ".claude-plugin"), { recursive: true });
  await writeFile(
    join(market, ".claude-plugin", "marketplace.json"),
    json({
      name,
      owner: { name: "Dev" },
      plugins: plugins.map((plugin) => ({ name: plugin, source: `./${plugin}` })),
    }),
  );
}

/** 只有一个插件 `hang` 的 zip（store 方式，不压缩）。 */
function storedZip(files) {
  const crcTable = Array.from({ length: 256 }, (_, n) => {
    let c = n;
    for (let k = 0; k < 8; k += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    return c >>> 0;
  });
  const crc32 = (buffer) => {
    let c = 0xffffffff;
    for (const byte of buffer) c = crcTable[(c ^ byte) & 0xff] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  };
  const locals = [];
  const centrals = [];
  let offset = 0;
  for (const [name, text] of files) {
    const data = Buffer.from(text);
    const nameBytes = Buffer.from(name);
    const crc = crc32(data);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(data.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(nameBytes.length, 26);
    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50, 0);
    central.writeUInt16LE(20, 4);
    central.writeUInt16LE(20, 6);
    central.writeUInt32LE(crc, 16);
    central.writeUInt32LE(data.length, 20);
    central.writeUInt32LE(data.length, 24);
    central.writeUInt16LE(nameBytes.length, 28);
    central.writeUInt32LE(offset, 42);
    locals.push(local, nameBytes, data);
    centrals.push(central, nameBytes);
    offset += local.length + nameBytes.length + data.length;
  }
  const centralSize = centrals.reduce((size, part) => size + part.length, 0);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(files.length, 8);
  end.writeUInt16LE(files.length, 10);
  end.writeUInt32LE(centralSize, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, ...centrals, end]);
}

/** 本机 HTTP 服务：`hang` 时收下请求、不回（通知 `arrived`），否则回 zip。 */
async function zipServer(zip) {
  const state = { hang: true, waiters: [], held: [] };
  const server = createServer((request, response) => {
    for (const waiter of state.waiters.splice(0)) waiter();
    if (state.hang) {
      state.held.push(response);
      return;
    }
    response.writeHead(200, { "content-length": zip.length });
    response.end(zip);
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  return {
    url: `http://127.0.0.1:${server.address().port}/hang.zip`,
    arrived: () => new Promise((resolve) => state.waiters.push(resolve)),
    serve: () => {
      state.hang = false;
    },
    async close() {
      for (const response of state.held) response.destroy();
      server.closeAllConnections();
      server.close();
    },
  };
}

export async function checkPluginStore({ nodeRoot, base, expectEqual, zcode }) {
  const home = join(base, "home");
  await mkdir(home, { recursive: true });
  await mkdir(join(base, "work", ".git"), { recursive: true });
  const work = realpathSync(join(base, "work"));
  const market = join(realpathSync(base), "mkt");
  const store = join(realpathSync(home), ".zcode", "cli", "plugins");
  const env = { HOME: home, USERPROFILE: home, PATH: process.env.PATH };
  const names = [
    "alpha",
    "beta",
    "gamma",
    "delta",
    "epsilon",
    "zeta",
    "eta",
    "theta",
    "iota",
    "kappa",
    "lambda",
  ];
  for (const name of names) await writePlugin(market, name, "1.0.0");
  await writeCatalog(market, "mkt", names);

  const rust = async (...args) => {
    try {
      const { stdout } = await promisify(execFile)(zcode, ["plugin", ...args], { cwd: work, env });
      return { code: 0, stdout };
    } catch (error) {
      return { code: error.code, stdout: error.stdout, stderr: error.stderr };
    }
  };
  const loader = pathToFileURL(join(nodeRoot, "node_modules/tsx/dist/loader.mjs")).href;
  const workerScript = join(import.meta.dirname, "node-interop-plugin-store-worker.mjs");
  const worker = (command, input) => {
    const child = spawn(
      process.execPath,
      [
        "--no-warnings",
        "--import",
        loader,
        workerScript,
        "--node-root",
        nodeRoot,
        command,
        JSON.stringify(input),
      ],
      { cwd: work, env, stdio: ["ignore", "pipe", "pipe"] },
    );
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));
    const done = once(child, "close").then(([code, signal]) => {
      if (code !== 0) return { code, signal, stderr };
      return JSON.parse(stdout.trim().split("\n").pop());
    });
    return { child, done };
  };
  const nodePlugins = await importNode(nodeRoot, "bootstrap/src/plugins.ts");
  const { loadMarketplaceManifestSync } = await importNode(
    nodeRoot,
    "adapters/src/plugins/marketplace.ts",
  );
  const nodeOptions = { workingDirectory: work, env, pluginPreparation: NO_SEED };
  const nodeList = () => withHome(home, () => nodePlugins.listZCodePlugins(nodeOptions));
  const records = async () =>
    JSON.parse(await readFile(join(store, "installed_plugins.json"), "utf8")).plugins;
  const recordOf = async (id) => (await records()).find((record) => record.id === id);

  // 1. rust 装的，Node 能加载、能卸载。
  await rust("marketplace", "add", market);
  const installed = await rust("install", "alpha@mkt");
  const alpha = await recordOf("alpha@mkt");
  const loaded = (await nodeList()).plugins.find((plugin) => plugin.id === "alpha@mkt");
  expectEqual(
    "Node loads the plugin rust installed into its immutable directory",
    {
      code: installed.code,
      enabled: loaded?.enabled,
      rootPath: loaded?.rootPath,
      immutable: /\/1\.0\.0-[0-9a-f]{12}$/.test(alpha.installPath),
    },
    { code: 0, enabled: true, rootPath: alpha.installPath, immutable: true },
  );
  const removed = await withHome(home, () =>
    nodePlugins.uninstallZCodeMarketplacePlugin({ ...nodeOptions, pluginId: "alpha@mkt" }),
  );
  expectEqual(
    "Node uninstalls the plugin rust installed",
    {
      removed: removed?.id,
      record: await recordOf("alpha@mkt"),
      dir: await exists(alpha.installPath),
    },
    { removed: "alpha@mkt", record: undefined, dir: false },
  );

  // 2. Node 装的，rust 能加载、能更新。
  const nodeInstall = await worker("install", { marketplace: "mkt", name: "beta" }).done;
  const nodeBeta = await recordOf("beta@mkt");
  const rustList = JSON.parse((await rust("list", "--json")).stdout);
  await writePlugin(market, "beta", "1.1.0");
  const updated = await rust("update", "beta@mkt");
  const rustBeta = await recordOf("beta@mkt");
  const afterUpdate = (await nodeList()).plugins.find((plugin) => plugin.id === "beta@mkt");
  expectEqual(
    "rust loads and updates the plugin Node installed",
    {
      nodeOk: nodeInstall.diagnostics?.length === 0,
      rustSees: rustList.find((plugin) => plugin.id === "beta@mkt")?.rootPath,
      updated: updated.stdout,
      version: rustBeta.version,
      immutable: /\/1\.1\.0-[0-9a-f]{12}$/.test(rustBeta.installPath),
      installedAt: rustBeta.installedAt,
      nodeSees: [afterUpdate?.rootPath, afterUpdate?.version],
      oldOrphaned: await exists(join(nodeBeta.installPath, ".orphaned_at")),
    },
    {
      nodeOk: true,
      rustSees: nodeBeta.installPath,
      updated: "Updated plugin beta@mkt from 1.0.0 to 1.1.0. Restart zcode to apply.\n",
      version: "1.1.0",
      immutable: true,
      installedAt: nodeBeta.installedAt,
      nodeSees: [rustBeta.installPath, "1.1.0"],
      oldOrphaned: true,
    },
  );

  // 3. 两个进程并发安装不丢记录。
  for (const [left, right] of [
    ["gamma", "zeta"],
    ["delta", "eta"],
    ["epsilon", "theta"],
  ]) {
    await Promise.all([
      rust("install", `${left}@mkt`),
      worker("install", { marketplace: "mkt", name: right }).done,
    ]);
  }
  const ids = (await records()).map((record) => record.id).sort();
  expectEqual("concurrent rust and Node installs keep every record", ids, [
    "beta@mkt",
    "delta@mkt",
    "epsilon@mkt",
    "eta@mkt",
    "gamma@mkt",
    "theta@mkt",
    "zeta@mkt",
  ]);

  // 4. Node 持存储锁下载时被杀：rust 回收锁、照样装。
  const zip = storedZip([
    ["hang/.claude-plugin/plugin.json", json({ name: "hang", version: "1.0.0" })],
  ]);
  const server = await zipServer(zip);
  const { createHash } = await import("node:crypto");
  await mkdir(join(base, "zips"), { recursive: true });
  await writeFile(
    join(base, "zips", "marketplace.json"),
    json({
      name: "zips",
      plugins: [
        {
          name: "hang",
          source: {
            source: "url",
            type: "zip",
            url: server.url,
            sha256: createHash("sha256").update(zip).digest("hex"),
          },
        },
      ],
    }),
  );
  try {
    await rust("marketplace", "add", join(base, "zips", "marketplace.json"));
    const arrived = server.arrived();
    const nodeHang = worker("install", { marketplace: "zips", name: "hang" });
    await arrived;
    const nodeHeldLock = await exists(join(store, "installed_plugins.json.lock"));
    nodeHang.child.kill("SIGKILL");
    await nodeHang.done;
    const afterNodeCrash = await rust("install", "iota@mkt");
    expectEqual(
      "rust reclaims the storage lock of a Node process killed while downloading",
      { nodeHeldLock, code: afterNodeCrash.code, record: (await recordOf("iota@mkt"))?.id },
      { nodeHeldLock: true, code: 0, record: "iota@mkt" },
    );

    // 5. rust 在锁外暂存时被杀；rust 持锁时崩溃（锁目录里留下已死的 owner）：Node 照样装。
    const rustArrived = server.arrived();
    const rustHang = spawn(zcode, ["plugin", "install", "hang@zips"], {
      cwd: work,
      env,
      stdio: "ignore",
    });
    await rustArrived;
    const hangDir = join(store, "cache", "zips", "hang");
    const staged = (await readdir(hangDir)).filter((name) => name.includes(".rust-stage-"));
    rustHang.kill("SIGKILL");
    await once(rustHang, "close");
    const afterRustCrash = await worker("install", { marketplace: "mkt", name: "kappa" }).done;
    const lockDir = join(store, "installed_plugins.json.lock");
    await mkdir(lockDir, { recursive: true });
    await writeFile(
      join(lockDir, "owner-dead-rust.json"),
      `${JSON.stringify({ pid: DEAD_PID, createdAt: Date.now(), token: "dead-rust" })}\n`,
    );
    const afterRustLock = await worker("install", { marketplace: "mkt", name: "lambda" }).done;
    server.serve();
    const retried = await rust("install", "hang@zips");
    const leftover = (await readdir(hangDir)).filter((name) => staged.includes(name));
    expectEqual(
      "Node continues after rust crashed while staging or holding the lock; rust sweeps its dead stage",
      {
        staged: staged.length,
        kappa: afterRustCrash.diagnostics?.length,
        lambda: afterRustLock.diagnostics?.length,
        retried: retried.code,
        leftover,
      },
      { staged: 1, kappa: 0, lambda: 0, retried: 0, leftover: [] },
    );
  } finally {
    await server.close();
  }

  // 6. Node 提交之后、收尾之前崩溃：rust 刷新这个市场时先收尾，Node 之后读到 rust 写的新清单。
  const txSource = join(realpathSync(base), "tx-mkt");
  await mkdir(txSource, { recursive: true });
  await writeFile(
    join(txSource, "marketplace.json"),
    json({
      name: "mkt",
      plugins: [...names, "from-node"].map((name) => ({ name, source: `./${name}` })),
    }),
  );
  const target = join(store, "marketplaces", "mkt");
  const deadTx = await worker("dead-tx", {
    target,
    source: txSource,
    authority: join(store, "known_marketplaces.json"),
    marketplace: "mkt",
  }).done;
  const leftBehind = [
    await exists(join(store, "marketplaces", ".mkt.backup")),
    await exists(join(store, "marketplaces", ".mkt.transaction.json")),
  ];
  await writePlugin(market, "from-rust", "1.0.0");
  await writeCatalog(market, "mkt", [...names, "from-rust"]);
  const refreshed = await rust("marketplace", "update", "mkt");
  const nodeView = withHome(home, () => loadMarketplaceManifestSync(store, "mkt"));
  expectEqual(
    "rust settles Node's dead marketplace transaction before replacing the cache",
    {
      transaction: typeof deadTx.transactionId,
      leftBehind,
      refreshed: refreshed.code,
      cleaned: [
        await exists(join(store, "marketplaces", ".mkt.backup")),
        await exists(join(store, "marketplaces", ".mkt.transaction.json")),
      ],
      nodeSees: (await nodeView)?.plugins.map((plugin) => plugin.name).at(-1),
    },
    {
      transaction: "string",
      leftBehind: [true, true],
      refreshed: 0,
      cleaned: [false, false],
      nodeSees: "from-rust",
    },
  );
}

// ---------------------------------------------------------------- 单独运行（node node-interop-plugin-store.mjs [--node-root …]）

const isMain = process.argv[1] && resolve(process.argv[1]) === import.meta.filename;
if (isMain) {
  // HOME 或 `ZCODE_*` 指向真实用户目录时什么都不做（node-store.md §6.1）
  refuseRealHome("node-interop-plugin-store");
  const { nodeRoot, args } = nodeRootFromArgs(process.argv.slice(2));
  ensureTypeScript(import.meta.filename, nodeRoot, args);
  const failures = [];
  const expectEqual = (name, actual, expected) => {
    console.log(
      `${isDeepStrictEqual(actual, expected) ? "ok" : "MISMATCH"}: ${name}\n  ${JSON.stringify(actual)}`,
    );
    if (isDeepStrictEqual(actual, expected)) return;
    failures.push(
      `${name}\n  actual: ${JSON.stringify(actual)}\n  expected: ${JSON.stringify(expected)}`,
    );
  };
  const { zcode } = buildRuntimeBinaries();
  const base = await mkdtemp(join(tmpdir(), "rust-cli-node-plugin-store-"));
  try {
    await checkPluginStore({ nodeRoot, base, expectEqual, zcode });
  } finally {
    await rm(base, { recursive: true, force: true });
  }
  if (failures.length) {
    console.error(
      `node-interop-plugin-store: ${failures.length} mismatch(es)\n${failures.join("\n")}`,
    );
    process.exitCode = 1;
  } else {
    console.log("node-interop-plugin-store: OK");
  }
}
