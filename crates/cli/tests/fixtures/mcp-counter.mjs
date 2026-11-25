#!/usr/bin/env node
// app_server_workspaces.rs 的 stdio MCP 服务器：每起一个进程，在日志文件里记一行 `start <名字> <pid> <cwd>`；输入关闭时
// 记 `exit <名字> <pid>` 再退出。列出一个工具 `probe`。参数：日志文件、服务器名。
import { appendFileSync } from "node:fs";
import { createInterface } from "node:readline";

const [log, name] = process.argv.slice(2);
appendFileSync(log, `start ${name} ${process.pid} ${process.cwd()}\n`);

function result(method, params) {
  switch (method) {
    case "initialize":
      return {
        protocolVersion: params?.protocolVersion ?? "2025-06-18",
        capabilities: { tools: {} },
        serverInfo: { name, version: "1.0.0" },
      };
    case "tools/list":
      return {
        tools: [
          {
            name: "probe",
            description: `Probe the ${name} server.`,
            inputSchema: { type: "object", properties: {} },
          },
        ],
      };
    case "tools/call":
      return { content: [{ type: "text", text: `${name} ${process.pid}` }] };
    default:
      return undefined;
  }
}

const lines = createInterface({ input: process.stdin });
lines.on("line", (line) => {
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    return;
  }
  if (message.id === undefined) return;
  const value = result(message.method, message.params);
  const reply =
    value === undefined
      ? { jsonrpc: "2.0", id: message.id, error: { code: -32601, message: "Method not found" } }
      : { jsonrpc: "2.0", id: message.id, result: value };
  process.stdout.write(`${JSON.stringify(reply)}\n`);
});
lines.on("close", () => {
  appendFileSync(log, `exit ${name} ${process.pid}\n`);
  process.exit(0);
});
