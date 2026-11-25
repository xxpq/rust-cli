//! 工作区声明的 MCP 服务器的 digest（docs/specs/rust-cli/ext.md §3.1 与 18-mcp-trust.md §12.2）：与 Node `workspace-mcp-trust` 的 `version: 1`
//! 逐字节相同，Node CLI、App 与本工程的信任记录互相认。
//!
//! 照搬 `apps/zcode-cli/packages/bootstrap/src/workspace-mcp-trust/index.ts` 的 `digest`、`executionConfig`、`hash`；stdio 的相对 `cwd`
//! 按声明来源的基准目录解析（项目配置文件：`adapters/src/config/project-config.adapter.ts` 的 `normalizeProjectMcpServer`；回落的
//! `.agents/mcp.json`：工作区根）；`cmd /c` 的拆法照 `packages/shared/src/mcp.ts` 的 `normalizeMcpStdioCommand`。纯函数：入口给出它读到的
//! 原文与路径。

use rust_cli_base::hash::sha256_hex;
use rust_cli_base::js::json::stringify;
use serde_json::{Map, Value, json};

use super::node_config::{self, normalize};

/// Node 的 digest 版本（`hash({version: 1, …})`）。
const DIGEST_VERSION: u64 = 1;
/// Windows 上被拆开的外壳：`cmd` / `cmd.exe` 加 `/c`（比较时命令名转小写）。
const CMD: [&str; 2] = ["cmd", "cmd.exe"];
const CMD_RUN: &str = "/c";

/// 算一个声明的 digest 要的输入（都是 Node 那边同名的值）。
pub struct Declaration<'a> {
    /// 工作区身份（Host 没给时是规范化的工作区根）。
    pub identity: &'a str,
    /// 规范化的工作区根（Node `resolve(workingDirectory)`）。
    pub root: &'a str,
    pub name: &'a str,
    /// 声明的来源文件：项目配置的声明是全部已加载的项目配置文件（发现顺序），回落的声明只有 `.agents/mcp.json`。
    pub source_paths: &'a [String],
    /// stdio 的相对 `cwd` 的基准目录（[`node_config::project_cwd`]）：项目配置文件是 [`node_config::config_dir`]，回落的是工作区根。
    pub base: &'a str,
    /// 声明所在文件的服务器表里 `<name>` 的原文。
    pub server: &'a Map<String, Value>,
    /// Node 在 Windows 上把 `cmd /c x …` 拆成 `x …`（`process.platform === "win32"`）。
    pub windows: bool,
}

/// `sha256(JSON.stringify(按键排序的 {version, workspaceIdentity, workingDirectory, serverName, sourcePaths, config}))`。
pub fn declaration_digest(declaration: &Declaration<'_>) -> String {
    let payload = json!({
        "version": DIGEST_VERSION,
        "workspaceIdentity": declaration.identity,
        "workingDirectory": declaration.root,
        "serverName": declaration.name,
        "sourcePaths": declaration.source_paths,
        "config": execution_config(declaration),
    });
    sha256_hex(stringify(&sorted(&payload)).as_bytes())
}

/// Node `executionConfig`（声明先经 `normalizeMcpServerConfigInput` 与项目级的 `cwd` 规则）：去掉 `enabled`、`source`；stdio 补
/// `command`、`args`、`env`、`cwd`，http / sse 补 `headers`。
fn execution_config(declaration: &Declaration<'_>) -> Value {
    let mut config = normalize(declaration.server);
    config.remove("enabled");
    config.remove("source");
    if config.get("type").and_then(Value::as_str) == Some("stdio") {
        let (command, args) = stdio_command(&config, declaration.windows);
        let cwd = project_cwd(&config, declaration);
        config.insert("command".into(), command);
        config.insert("args".into(), args);
        let env = config.remove("env").unwrap_or_else(|| json!({}));
        config.insert("env".into(), env);
        config.insert("cwd".into(), Value::String(cwd));
    } else {
        let headers = config.remove("headers").unwrap_or_else(|| json!({}));
        config.insert("headers".into(), headers);
    }
    Value::Object(config)
}

/// Node `normalizeMcpStdioCommand(command, args = [])`。
fn stdio_command(config: &Map<String, Value>, windows: bool) -> (Value, Value) {
    let command = config.get("command").cloned().unwrap_or(Value::Null);
    let args: Vec<Value> = config
        .get("args")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let wrapped = windows
        && command
            .as_str()
            .is_some_and(|name| CMD.contains(&name.to_lowercase().as_str()))
        && args.first().and_then(Value::as_str) == Some(CMD_RUN);
    match args.get(1) {
        // Node 判的是 `args[1]` 为真：空串不拆
        Some(Value::String(inner)) if wrapped && !inner.is_empty() => (
            Value::String(inner.clone()),
            Value::Array(args[2..].to_vec()),
        ),
        _ => (command, Value::Array(args)),
    }
}

/// 项目级的 `cwd`：与执行时的同一条规则（[`node_config::project_cwd`]），审核、digest、执行看到的是同一个目录。
fn project_cwd(config: &Map<String, Value>, declaration: &Declaration<'_>) -> String {
    node_config::project_cwd(declaration.base, config.get("cwd").and_then(Value::as_str))
}

/// Node `hash` 的 replacer：每个对象的键按 `Array.prototype.sort`（UTF-16 码元序）重排，数组不动。
fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
            Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), sorted(&map[key])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
#[path = "declaration_tests.rs"]
mod tests;
