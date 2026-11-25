//! Node CLI 的用户级配置 `~/.zcode/cli/config.json`（docs/specs/rust-cli/config.md §1）。
//!
//! 读：只负责读文件并判断是不是 JSON 对象；`permission`、`hooks`、`mcp` 各段由入口层交给各能力 crate
//! 的解析器。文件可以没有；读不出或不是对象时整份忽略（Node `config_file_invalid`）。
//! 写：界面语言 `ui.locale`（tui.md §7.1，Node `updateUiLocaleInFileConfig`）、主题 `ui.theme`
//! （tui-manage.md §2.1，同一种写法）与 `/mcp enable/disable` 的 `enabled`（tui-manage.md §6.1），都经
//! [`update_cli_config`] 这一个写者。

use std::io;

use rust_cli_fs::LockOptions;
use rust_cli_paths::{StoragePath, StoragePaths};
use serde_json::{Map, Value};

use rust_cli_base::ui_prefs::{UiLocale, UiTheme};

use crate::credentials::PRIVATE_WRITE;

/// 写配置失败；文案与 Node `file-config.adapter.ts` 相同。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigWriteError {
    #[error("Unable to read config file: {0}")]
    Read(String),
    #[error("Unable to parse config file as JSON: {0}")]
    Parse(String),
    #[error("Config file must contain a JSON object: {0}")]
    NotObject(String),
    #[error("Unable to write config file: {0}")]
    Write(String),
    /// `/mcp enable/disable`：这个文件里没有这个服务器（tui-manage.md §6.1）。
    #[error("MCP server {0} is not defined in {1}")]
    NoMcpServer(String, String),
}

/// 读一份配置文件的结果。
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigFile {
    /// 文件不存在。
    Missing,
    /// 读不出、不是 JSON 或不是对象；`reason` 进警告。
    Invalid {
        reason: String,
    },
    Loaded(Map<String, Value>),
}

impl ConfigFile {
    /// 解析读到的字节（项目级文件经执行环境读到后也用它判断）。
    pub fn parse(bytes: &[u8]) -> Self {
        match serde_json::from_slice::<Value>(bytes) {
            Ok(Value::Object(object)) => Self::Loaded(object),
            Ok(_) => Self::Invalid {
                reason: "the file is not a JSON object".to_owned(),
            },
            Err(error) => Self::Invalid {
                reason: error.to_string(),
            },
        }
    }

    /// 读到的对象；没有或无效时为 None。
    pub fn object(&self) -> Option<&Map<String, Value>> {
        match self {
            Self::Loaded(object) => Some(object),
            Self::Missing | Self::Invalid { .. } => None,
        }
    }
}

/// 读用户级配置（每次都以磁盘为准，不缓存）。
pub async fn read_cli_config(paths: &StoragePaths) -> ConfigFile {
    read_config_file(&paths.cli_config()).await
}

/// 读用户级的 MCP 声明回落 `~/.agents/mcp.json`（docs/specs/rust-cli/ext.md §3.1；每次都以磁盘为准）。
pub async fn read_user_agents_mcp(paths: &StoragePaths) -> ConfigFile {
    read_config_file(&paths.user_agents_mcp()).await
}

async fn read_config_file(file: &StoragePath) -> ConfigFile {
    match rust_cli_fs::read_optional(file.as_path()).await {
        Ok(Some(bytes)) => ConfigFile::parse(&bytes),
        Ok(None) => ConfigFile::Missing,
        Err(error) => ConfigFile::Invalid {
            reason: error.to_string(),
        },
    }
}

/// 用户级配置的唯一写入口：持 Node 形状的目录锁（`config.json.lock/`，fs 的 `with_file_lock`）读、改、
/// 原子写（0600）。`edit` 收文件的字节（不存在时为 None）与错误里显示的路径，返回新的文件文字。返回写入的
/// 文件。
///
/// 缺陷修复 R2-12：`ui.locale`、`ui.theme` 与 MCP 的 `enabled` 原来各自读改写、不加锁；TUI 里这些写回并发跑
/// （`/theme` 紧接着 `/mcp disable`）时后写的覆盖先写的。现在都经这里，同一进程与多个进程之间都按锁串行。
/// 写出的文字不变（Node 互操作逐字节相同）；Node 写这个文件不加锁，锁只在本工程的写者之间起作用。
async fn update_cli_config(
    paths: &StoragePaths,
    edit: impl FnOnce(Option<&[u8]>, &str) -> Result<String, ConfigWriteError>,
) -> Result<StoragePath, ConfigWriteError> {
    update_user_file(paths.cli_config(), edit).await
}

/// 同 [`update_cli_config`]，文件由调用方给（用户级的 `~/.agents/mcp.json` 也经这里）。
async fn update_user_file(
    file: StoragePath,
    edit: impl FnOnce(Option<&[u8]>, &str) -> Result<String, ConfigWriteError>,
) -> Result<StoragePath, ConfigWriteError> {
    let path = file.as_path();
    let shown = path.display().to_string();
    let written = rust_cli_fs::with_file_lock(path, &LockOptions::default(), async || {
        let Ok(bytes) = rust_cli_fs::read_optional(path).await else {
            return Ok(Err(ConfigWriteError::Read(shown.clone())));
        };
        let text = match edit(bytes.as_deref(), &shown) {
            Ok(text) => text,
            Err(error) => return Ok(Err(error)),
        };
        let written = rust_cli_fs::atomic_write(path, text.as_bytes(), PRIVATE_WRITE).await;
        Ok::<_, io::Error>(written.map_err(|_| ConfigWriteError::Write(shown.clone())))
    })
    .await;
    match written {
        Ok(Ok(())) => Ok(file),
        Ok(Err(error)) => Err(error),
        // 拿不到锁（超时）或建不了目录：同写失败。
        Err(_) => Err(ConfigWriteError::Write(shown)),
    }
}

/// 改一份 Node 配置文件的顶层对象（插件段，docs/specs/rust-cli/plugin-store.md §4、§10）：用户级配置（`paths.cli_config()`）
/// 或项目配置 `<工作区>/.zcode/config.json`。持 `config.json.lock/`（Node 的 `withFileLock(config.json)` 同一把；插件写的调用方
/// 已持插件存储锁，锁序见 plugin-store.md §4）读改写；文件不在时从空对象开始。`edit` 返回改没改，没改时不写（Node
/// `removePluginFromFileConfig` 等相同）。读不出、不是对象时报错、不写。写法同用户级配置：原子写，0600，两格缩进加换行。
pub async fn update_config_object(
    path: &std::path::Path,
    edit: impl FnOnce(&mut Map<String, Value>) -> bool,
) -> Result<(), ConfigWriteError> {
    let shown = path.display().to_string();
    let written = rust_cli_fs::with_file_lock(path, &LockOptions::default(), async || {
        let Ok(bytes) = rust_cli_fs::read_optional(path).await else {
            return Ok(Err(ConfigWriteError::Read(shown.clone())));
        };
        let mut object = match bytes.as_deref().map(serde_json::from_slice::<Value>) {
            None => Map::new(),
            Some(Ok(Value::Object(object))) => object,
            Some(Ok(_)) => return Ok(Err(ConfigWriteError::NotObject(shown.clone()))),
            Some(Err(_)) => return Ok(Err(ConfigWriteError::Parse(shown.clone()))),
        };
        if !edit(&mut object) {
            return Ok(Ok(()));
        }
        let mut text = node_pretty(&Value::Object(object));
        text.push('\n');
        let written = rust_cli_fs::atomic_write(path, text.as_bytes(), PRIVATE_WRITE).await;
        Ok::<_, io::Error>(written.map_err(|_| ConfigWriteError::Write(shown.clone())))
    })
    .await;
    match written {
        Ok(result) => result,
        Err(_) => Err(ConfigWriteError::Write(shown)),
    }
}

/// 把 `ui.locale` 写回用户级配置（Node `updateUiLocaleInFileConfig`，tui.md §7.1）：读改写，其余字段与
/// 键序不变；文件读不出或不是对象时报错、不写。返回写入的文件。
pub async fn write_ui_locale(
    paths: &StoragePaths,
    locale: UiLocale,
) -> Result<StoragePath, ConfigWriteError> {
    write_ui_value(paths, "locale", locale.as_str()).await
}

/// 把 `ui.theme` 写回用户级配置（tui-manage.md §2.1）：与 `ui.locale` 同一套写法。
pub async fn write_ui_theme(
    paths: &StoragePaths,
    theme: UiTheme,
) -> Result<StoragePath, ConfigWriteError> {
    write_ui_value(paths, "theme", theme.as_str()).await
}

async fn write_ui_value(
    paths: &StoragePaths,
    key: &str,
    value: &str,
) -> Result<StoragePath, ConfigWriteError> {
    update_cli_config(paths, |bytes, shown| {
        let mut object = match bytes.map(serde_json::from_slice::<Value>) {
            None => Map::new(),
            Some(Ok(Value::Object(object))) => object,
            Some(Ok(_)) => return Err(ConfigWriteError::NotObject(shown.to_owned())),
            Some(Err(_)) => return Err(ConfigWriteError::Parse(shown.to_owned())),
        };
        patch_ui(&mut object, key, value);
        let mut text = node_pretty(&Value::Object(object));
        text.push('\n');
        Ok(text)
    })
    .await
}

/// Node `patchUiLocale`：`{...parsed, ui: {...currentUi, <key>: 值}}`。已有的键保持位置（serde_json 按插入
/// 顺序），没有的加在末尾；`ui` 不是对象时换成只有这个键的对象。
fn patch_ui(object: &mut Map<String, Value>, key: &str, value: &str) {
    let ui = object
        .entry("ui")
        .or_insert_with(|| Value::Object(Map::new()));
    if !ui.is_object() {
        *ui = Value::Object(Map::new());
    }
    if let Value::Object(ui) = ui {
        ui.insert(key.to_owned(), Value::String(value.to_owned()));
    }
}

/// Node 配置文件里服务器表的键、`.agents/mcp.json` 里服务器表的键。
const CONFIG_MCP_TABLE: [&str; 2] = ["mcp", "servers"];
const AGENTS_MCP_TABLE: [&str; 1] = ["mcpServers"];

/// 一个文件里 MCP 服务器表的位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpTable {
    /// Node 配置文件（用户级配置、项目配置）的 `mcp.servers`。
    Config,
    /// `.agents/mcp.json`（工作区根的、home 下的回落文件）的 `mcpServers`。
    Agents,
}

impl McpTable {
    /// 从文件的顶层对象到表的键。
    pub fn path(self) -> &'static [&'static str] {
        match self {
            Self::Config => &CONFIG_MCP_TABLE,
            Self::Agents => &AGENTS_MCP_TABLE,
        }
    }

    /// 文件里的表（还没校验是不是对象）。
    pub fn of(self, object: &Map<String, Value>) -> Option<&Value> {
        let (first, rest) = self.path().split_first()?;
        rest.iter()
            .try_fold(object.get(*first)?, |value, key| value.get(*key))
    }

    /// 警告里的写法（`mcp.servers`、`mcpServers`）。
    pub fn name(self) -> String {
        self.path().join(".")
    }

    /// 用户级的这种文件：用户级配置、`~/.agents/mcp.json`。
    fn user_file(self, paths: &StoragePaths) -> StoragePath {
        match self {
            Self::Config => paths.cli_config(),
            Self::Agents => paths.user_agents_mcp(),
        }
    }
}

/// `/mcp enable/disable` 的写法（docs/specs/rust-cli/tui-manage.md §6.1，Node 的 `enabled` 字段）：
/// 文件的服务器表 `table` 里 `<名字>.enabled` 设为开关（已有的键原位，没有时加在末尾），去掉旧字段 `enable`；返回新的
/// 文件文字（两格缩进加换行）。`bytes` 为 None 表示文件不存在。`shown` 是错误里显示的路径。
pub fn mcp_enabled_text(
    bytes: Option<&[u8]>,
    shown: &str,
    table: McpTable,
    name: &str,
    enabled: bool,
) -> Result<String, ConfigWriteError> {
    let missing = || ConfigWriteError::NoMcpServer(name.to_owned(), shown.to_owned());
    let mut object = match bytes.map(serde_json::from_slice::<Value>) {
        None => return Err(missing()),
        Some(Ok(Value::Object(object))) => object,
        Some(Ok(_)) => return Err(ConfigWriteError::NotObject(shown.to_owned())),
        Some(Err(_)) => return Err(ConfigWriteError::Parse(shown.to_owned())),
    };
    let (first, rest) = table.path().split_first().ok_or_else(missing)?;
    let server = rest
        .iter()
        .fold(object.get_mut(*first), |value, key| value?.get_mut(*key))
        .and_then(|servers| servers.get_mut(name))
        .and_then(Value::as_object_mut)
        .ok_or_else(missing)?;
    server.insert("enabled".to_owned(), Value::Bool(enabled));
    server.shift_remove("enable");
    let mut text = node_pretty(&Value::Object(object));
    text.push('\n');
    Ok(text)
}

/// 用户级文件里的 `/mcp enable/disable`（tui-manage.md §6.1）：`table` 定是哪个文件（用户级配置、`~/.agents/mcp.json`），
/// 经与用户级配置同一个持锁写者读改写，原子写，0600。返回写入的文件。
pub async fn write_mcp_enabled(
    paths: &StoragePaths,
    table: McpTable,
    name: &str,
    enabled: bool,
) -> Result<StoragePath, ConfigWriteError> {
    update_user_file(table.user_file(paths), |bytes, shown| {
        mcp_enabled_text(bytes, shown, table, name, enabled)
    })
    .await
}

/// `compact.autoThresholdTokens` 的写法（Puffin 的 `/autocompact` 与设置页，compact.md §2.3；同 Node 的
/// `updateJsonObjectFile`）：`Some(n)` 设成 n（已有的键原位，没有时加在末尾），`None` 删掉它，`compact` 空了也删掉；
/// 其余字段与键序不变。文件不存在时从空对象开始；读不出或不是对象时报错、不写。
pub fn compact_at_text(
    bytes: Option<&[u8]>,
    shown: &str,
    at: Option<u64>,
) -> Result<String, ConfigWriteError> {
    let mut object = match bytes.map(serde_json::from_slice::<Value>) {
        None => Map::new(),
        Some(Ok(Value::Object(object))) => object,
        Some(Ok(_)) => return Err(ConfigWriteError::NotObject(shown.to_owned())),
        Some(Err(_)) => return Err(ConfigWriteError::Parse(shown.to_owned())),
    };
    match at {
        Some(tokens) => {
            let compact = object
                .entry("compact")
                .or_insert_with(|| Value::Object(Map::new()));
            if !compact.is_object() {
                *compact = Value::Object(Map::new());
            }
            if let Value::Object(compact) = compact {
                compact.insert("autoThresholdTokens".to_owned(), Value::from(tokens));
            }
        }
        None => {
            if let Some(Value::Object(compact)) = object.get_mut("compact") {
                compact.shift_remove("autoThresholdTokens");
                if compact.is_empty() {
                    object.shift_remove("compact");
                }
            }
        }
    }
    let mut text = node_pretty(&Value::Object(object));
    text.push('\n');
    Ok(text)
}

/// 用户级配置里的 `compact.autoThresholdTokens`：经 [`update_cli_config`] 读改写，原子写，0600。返回写入的文件。
pub async fn write_compact_at(
    paths: &StoragePaths,
    at: Option<u64>,
) -> Result<StoragePath, ConfigWriteError> {
    update_cli_config(paths, |bytes, shown| compact_at_text(bytes, shown, at)).await
}

/// `JSON.stringify(value, null, 2)`（定义在 base 的 `js::json::pretty`）。
pub(crate) use rust_cli_base::js::json::pretty as node_pretty;

#[cfg(test)]
#[path = "cli_config_tests.rs"]
mod tests;
