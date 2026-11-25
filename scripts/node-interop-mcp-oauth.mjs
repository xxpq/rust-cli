// rust-cli 与 Node 互用远程 MCP 服务器的 OAuth 令牌（docs/specs/rust-cli/ext.md §3.12.4）：node-interop-check.mjs 的第三十九段。
// 临时 HOME 与本地假服务（模型、MCP 服务器与它的授权服务器）；系统凭据库换成同一个 JSON 文件（Node 一侧注入
// `createSystemCredentialCipher` 的 `keyStore`，rust-cli 一侧是 debug 构建的 `_ZCODE_ASSUME_KEYRING_FILE`），不碰真实的钥匙串。
//
// A. Node 写、rust 用：Node 的 `publishCanonicalCredentials` 发布一对令牌（离过期还远），rust-cli 的 `zcode -p` 调 MCP 工具时带它。
// B. rust 刷新、Node 读：Node 发布的令牌快过期，rust-cli 先刷新（Node 的刷新锁、Node 的键），带新令牌调用；Node 的
//    `loadCredentialPair` 读出 rust 写的一对（同一个 client、新的 generation），Node 的运行期 `token()` 直接给出它、不再刷新。
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { GLM_5_3 } from "./fixture/model.mjs";
import { credentialsFile, run } from "./node-interop-app-login.mjs";

/** Node `SYSTEM_SERVICE`（systemCredentialCipher.ts）。 */
const SERVICE = "ZCode Credential Encryption";
const accountOf = (file) => createHash("sha256").update(resolve(file)).digest("hex");

/** 文件里的假凭据库：与 rust-cli 的 `FileKeyStore` 同一种格式（同第三十七段）。 */
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

const event = (type, data) => `event: ${type}\ndata: ${JSON.stringify({ type, ...data })}\n\n`;
const START = event("message_start", {
  message: {
    id: "m",
    type: "message",
    role: "assistant",
    model: "m",
    content: [],
    stop_reason: null,
    usage: { input_tokens: 1, output_tokens: 1 },
  },
});
const end = (stop) =>
  event("message_delta", { delta: { stop_reason: stop }, usage: { output_tokens: 1 } }) +
  event("message_stop", {});
const TOOL_CALL =
  START +
  event("content_block_start", {
    index: 0,
    content_block: { type: "tool_use", id: "toolu_1", name: "mcp__docs__ping", input: {} },
  }) +
  event("content_block_delta", {
    index: 0,
    delta: { type: "input_json_delta", partial_json: "{}" },
  }) +
  event("content_block_stop", { index: 0 }) +
  end("tool_use");
const DONE =
  START +
  event("content_block_start", { index: 0, content_block: { type: "text", text: "" } }) +
  event("content_block_delta", { index: 0, delta: { type: "text_delta", text: "done" } }) +
  event("content_block_stop", { index: 0 }) +
  end("end_turn");

/** 模型（先调 MCP 工具，再回 done）、MCP 服务器 `/mcp`（只认 `accepted` 里的令牌）与授权服务器 `/as`。 */
async function serve() {
  const state = { accepted: new Set(), seen: [], refreshes: [] };
  const server = createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => (body += chunk));
    request.on("end", () => {
      const base = `http://127.0.0.1:${server.address().port}`;
      const json = (status, value, headers = {}) => {
        response.writeHead(status, {
          "content-type": "application/json",
          connection: "close",
          ...headers,
        });
        response.end(value === undefined ? "" : JSON.stringify(value));
      };
      const path = new URL(request.url, base).pathname;
      if (path.endsWith("/v1/messages")) {
        const parsed = JSON.parse(body || "{}");
        if (!parsed.tools?.length) return json(200, { content: [{ type: "text", text: "title" }] });
        const answered = JSON.stringify(parsed.messages ?? []).includes("tool_result");
        response.writeHead(200, { "content-type": "text/event-stream", connection: "close" });
        return response.end(answered ? DONE : TOOL_CALL);
      }
      if (path === "/.well-known/oauth-protected-resource/mcp")
        return json(200, { resource: `${base}/mcp`, authorization_servers: [`${base}/as`] });
      if (path === "/.well-known/oauth-authorization-server/as")
        return json(200, {
          issuer: `${base}/as`,
          authorization_endpoint: `${base}/as/authorize`,
          token_endpoint: `${base}/as/token`,
          response_types_supported: ["code"],
        });
      if (path === "/as/token") {
        const form = new URLSearchParams(body);
        state.refreshes.push(Object.fromEntries(form));
        if (form.get("refresh_token") !== "rt-node") return json(400, { error: "invalid_grant" });
        state.accepted.add("at-rust");
        return json(200, {
          access_token: "at-rust",
          token_type: "Bearer",
          expires_in: 3600,
          refresh_token: "rt-rust",
        });
      }
      if (path === "/mcp") {
        const token = (request.headers.authorization ?? "").replace(/^Bearer /, "");
        state.seen.push(token);
        if (!state.accepted.has(token))
          return json(401, undefined, { "www-authenticate": "Bearer" });
        if (request.method === "GET") return json(405);
        const message = JSON.parse(body);
        if (message.id === undefined) return json(202);
        const result =
          message.method === "initialize"
            ? {
                protocolVersion: "2025-06-18",
                capabilities: { tools: {} },
                serverInfo: { name: "docs", version: "1" },
              }
            : message.method === "tools/list"
              ? { tools: [{ name: "ping", inputSchema: { type: "object" } }] }
              : { content: [{ type: "text", text: "pong" }] };
        return json(200, { jsonrpc: "2.0", id: message.id, result });
      }
      return json(404, {});
    });
  });
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  return { server, state, base: `http://127.0.0.1:${server.address().port}` };
}

export async function checkMcpOAuth({ nodeRoot, base, expectEqual, zcode }) {
  const adapters = (path) =>
    import(pathToFileURL(join(nodeRoot, "apps/zcode-cli/packages/adapters/src", path)).href);
  const { createSharedZCodeCredentialStore } = await adapters("auth/shared-credentials.ts");
  const { createCredentialKeyPrefix } = await adapters("mcp/oauth.ts");
  const { publishCanonicalCredentials, loadCredentialPair } = await adapters(
    "mcp/oauth-credentials.ts",
  );
  const { createMcpOAuthTokenProvider } = await adapters("mcp/oauth-provider.ts");
  const { createSystemCredentialCipher } = await import(
    pathToFileURL(
      join(nodeRoot, "packages/shared/src/credential-security/systemCredentialCipher.ts"),
    ).href
  );
  const { server, state, base: url } = await serve();
  const findings = {};
  try {
    const home = join(base, "home");
    await mkdir(join(home, "work/.git"), { recursive: true });
    const file = credentialsFile(home);
    const keyring = join(home, "keyring.json");
    // 每次一个新的 store：Node 按 keyStore 实例缓存密钥，相当于一个新进程。
    const store = () =>
      createSharedZCodeCredentialStore({
        filePath: file,
        cipher: createSystemCredentialCipher({
          filePath: file,
          env: {},
          keyStore: fileKeyStore(keyring, file),
        }),
      });
    const mcpUrl = `${url}/mcp`;
    const oauth = { type: "authorization_code" };
    const prefix = createCredentialKeyPrefix("docs", mcpUrl, oauth);
    const client = { client_id: "dyn-1" };
    const config = JSON.stringify({ mcpServers: { docs: { type: "http", url: mcpUrl } } });
    const env = {
      HOME: home,
      USERPROFILE: home,
      PATH: process.env.PATH,
      ZCODE_DATA_BASE_DIR: home,
      _ZCODE_ASSUME_KEYRING_FILE: keyring,
      ZCODE_MODEL_BASE_URL: url,
      ZCODE_API_KEY: "test-key",
      ZCODE_MODEL: GLM_5_3,
      ENABLE_TOOL_SEARCH: "false",
    };
    const print = () =>
      run(
        zcode,
        ["-p", "hi", "--allowedTools", "mcp__docs", "--strict-mcp-config", "--mcp-config", config],
        env,
        join(home, "work"),
      );

    // A. Node 写、rust 用。
    state.accepted.add("at-node");
    await publishCanonicalCredentials(store(), prefix, {
      clientInformation: client,
      publishedBy: "node",
      tokens: {
        access_token: "at-node",
        token_type: "Bearer",
        expires_in: 3600,
        refresh_token: "rt-node",
      },
    });
    const first = await print();
    expectEqual("mcp oauth A: rust-cli answers", "done", first.stdout.trim() || first.stderr);
    findings.rustUsesNodeToken =
      state.seen.length > 0 && state.seen.every((token) => token === "at-node");
    expectEqual(
      "mcp oauth A: every MCP request carries Node's token",
      true,
      findings.rustUsesNodeToken,
    );
    expectEqual("mcp oauth A: no refresh", 0, state.refreshes.length);

    // B. 快过期：rust 刷新、Node 读。
    state.accepted.clear();
    state.seen.length = 0;
    await publishCanonicalCredentials(store(), prefix, {
      clientInformation: client,
      publishedBy: "node",
      obtainedAt: Date.now() - 3_540_000,
      tokens: {
        access_token: "at-node-old",
        token_type: "Bearer",
        expires_in: 3600,
        refresh_token: "rt-node",
      },
    });
    const before = await loadCredentialPair(store(), prefix);
    const second = await print();
    expectEqual("mcp oauth B: rust-cli answers", "done", second.stdout.trim() || second.stderr);
    expectEqual(
      "mcp oauth B: one refresh with Node's refresh token",
      ["rt-node"],
      state.refreshes.map((r) => r.refresh_token),
    );
    expectEqual(
      "mcp oauth B: the refresh names the resource",
      mcpUrl,
      state.refreshes[0]?.resource,
    );
    expectEqual(
      "mcp oauth B: MCP requests carry the refreshed token",
      true,
      state.seen.length > 0 && state.seen.every((t) => t === "at-rust"),
    );
    const after = await loadCredentialPair(store(), prefix);
    findings.nodeReadsRustPair =
      after?.source === "canonical" && after?.tokens?.access_token === "at-rust";
    expectEqual(
      "mcp oauth B: Node reads the canonical pair rust-cli wrote",
      true,
      findings.nodeReadsRustPair,
    );
    expectEqual("mcp oauth B: same client", "dyn-1", after?.clientInformation?.client_id);
    expectEqual(
      "mcp oauth B: rust-cli rotated the refresh token",
      "rt-rust",
      after?.tokens?.refresh_token,
    );
    expectEqual(
      "mcp oauth B: a new generation",
      true,
      Boolean(after?.generation) && after.generation !== before?.generation,
    );
    const values = Object.entries(JSON.parse(await readFile(file, "utf8")))
      .filter(([key]) => key.startsWith(prefix))
      .map(([, value]) => value);
    expectEqual(
      "mcp oauth B: rust-cli sealed the pair with the system key",
      true,
      values.every((v) => v.startsWith("enc:v2:os:")),
    );
    const provider = createMcpOAuthTokenProvider({
      config: oauth,
      credentialStore: store(),
      keyPrefix: prefix,
      serverName: "docs",
      serverUrl: mcpUrl,
    });
    const refreshes = state.refreshes.length;
    expectEqual(
      "mcp oauth B: Node's runtime token() gives rust-cli's token",
      "at-rust",
      await provider.token(),
    );
    expectEqual("mcp oauth B: Node does not refresh it again", refreshes, state.refreshes.length);
  } finally {
    server.close();
  }
  return findings;
}
