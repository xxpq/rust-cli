//! 工作区声明的 digest 的结构（18-mcp-trust.md §12.2）；与 Node 逐字节的对拍在 cli 的 `config/mcp_trust_tests.rs`。
use super::*;

const ROOT: &str = "/Users/dev/repo";

fn server(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

/// 相对的 `cwd` 按 `base` 解析时的执行配置。
fn config_at(value: Value, base: &str, windows: bool) -> Value {
    let server = server(value);
    execution_config(&Declaration {
        identity: ROOT,
        root: ROOT,
        name: "x",
        source_paths: &[],
        base,
        server: &server,
        windows,
    })
}

/// 声明在项目配置文件 `file` 里时的执行配置（基准目录是 `config_dir(file)`）。
fn config_of(value: Value, file: &str, windows: bool) -> Value {
    config_at(value, &node_config::config_dir(file), windows)
}

#[test]
fn stdio_gains_args_env_and_an_absolute_cwd_and_loses_enabled() {
    let config = config_of(
        json!({"command": "uvx", "enable": false}),
        "/Users/dev/repo/zcode.json",
        false,
    );
    assert_eq!(
        config,
        json!({"type": "stdio", "command": "uvx", "args": [], "env": {}, "cwd": ROOT})
    );
}

#[test]
fn a_relative_cwd_is_resolved_from_the_directory_of_the_file_above_dot_zcode() {
    let file = "/Users/dev/repo/.zcode/config.json";
    let cwd = |value: Value| config_of(value, file, false)["cwd"].clone();
    assert_eq!(cwd(json!({"command": "a", "cwd": "../x"})), "/Users/dev/x");
    assert_eq!(
        cwd(json!({"command": "a", "cwd": "./tools/"})),
        "/Users/dev/repo/tools"
    );
    // 绝对路径原样（Node 不规范化它）
    assert_eq!(
        cwd(json!({"command": "a", "cwd": "/opt//mcp/"})),
        "/opt//mcp/"
    );
}

/// 回落的 `.agents/mcp.json`：相对的 `cwd` 按工作区根解析（Node 按 `workingDirectory`），不是 `.agents/`；没写是工作区根。
#[test]
fn a_relative_cwd_of_the_fallback_file_is_the_workspace_root() {
    let cwd = |value: Value| config_at(value, ROOT, false)["cwd"].clone();
    assert_eq!(
        cwd(json!({"command": "a", "cwd": "tools"})),
        "/Users/dev/repo/tools"
    );
    assert_eq!(cwd(json!({"command": "a"})), ROOT);
}

#[test]
fn cmd_c_is_unwrapped_only_on_windows() {
    let wrapped = json!({"command": "CMD.exe", "args": ["/c", "npx", "-y"]});
    let posix = config_of(wrapped.clone(), "/Users/dev/repo/zcode.json", false);
    assert_eq!(posix["command"], "CMD.exe");
    let windows = config_of(wrapped, "/Users/dev/repo/zcode.json", true);
    assert_eq!(
        (windows["command"].clone(), windows["args"].clone()),
        (json!("npx"), json!(["-y"]))
    );
    let empty = config_of(
        json!({"command": "cmd", "args": ["/c", ""]}),
        "/Users/dev/repo/zcode.json",
        true,
    );
    assert_eq!(empty["command"], "cmd");
}

#[test]
fn remote_servers_gain_headers_only() {
    let config = config_of(
        json!({"type": "remote", "url": "https://mcp.example.com", "http_headers": {"k": "v"}}),
        "/Users/dev/repo/zcode.json",
        false,
    );
    assert_eq!(
        config,
        json!({"type": "http", "url": "https://mcp.example.com", "headers": {"k": "v"}})
    );
}

/// JS 的 `Array.prototype.sort` 比 UTF-16 码元：代理对（U+1F600 的 0xD83D）排在 U+FF21 之前，UTF-8 的字节序相反。
#[test]
fn keys_are_sorted_by_utf16_code_units() {
    let value = sorted(&json!({"Ａ": 1, "😀": 2, "b": 3, "B": 4}));
    let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["B", "b", "😀", "Ａ"]);
}

#[test]
fn the_digest_changes_with_the_declaration_and_the_sources() {
    let one = server(json!({"command": "a"}));
    let two = server(json!({"command": "b"}));
    let sources = ["/Users/dev/repo/zcode.json".to_owned()];
    let digest = |server: &Map<String, Value>, sources: &[String]| {
        declaration_digest(&Declaration {
            identity: ROOT,
            root: ROOT,
            name: "x",
            source_paths: sources,
            base: ROOT,
            server,
            windows: false,
        })
    };
    assert_eq!(digest(&one, &sources).len(), 64);
    assert_ne!(digest(&one, &sources), digest(&two, &sources));
    assert_ne!(digest(&one, &sources), digest(&one, &[]));
}
