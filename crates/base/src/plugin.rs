//! 一个已加载插件的上下文与它的变量展开（docs/specs/rust-cli/ext.md §4.9、§4.10）。
//!
//! ext（skill、命令、MCP）、agents（agent 正文）与 hooks（命令与环境变量）都按插件替换 `${*_PLUGIN_ROOT}`、
//! `${*_PLUGIN_DATA}` 与 `${user_config.KEY}`。规则只写在这里：各处只选「哪个位置用哪条规则」（ext.md §4.10 的表）。
//! 只有纯函数，不做 IO。

use std::collections::BTreeMap;

use serde_json::Value;

use crate::names::vars;
use crate::secret::RedactedString;

/// `${user_config.KEY}` 的前缀（在 `${` 之后）。
const OPTION_PREFIX: &str = "user_config.";

/// 一个配置值（ext.md §4.9）：普通值是 JS `String(v)` 的文字；敏感值从读进来起就是秘密类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginOption {
    Plain(String),
    Secret(RedactedString),
}

impl PluginOption {
    /// 一个标量的配置值（字符串、数、布尔）；别的类型为 None（config.md D1：坏项跳过）。
    pub fn scalar(value: &Value) -> Option<String> {
        match value {
            Value::String(text) => Some(text.clone()),
            Value::Bool(_) | Value::Number(_) => Some(crate::js::json::stringify(value)),
            Value::Null | Value::Array(_) | Value::Object(_) => None,
        }
    }

    pub fn is_secret(&self) -> bool {
        matches!(self, Self::Secret(_))
    }

    /// 原值：只在交给插件进程（环境变量、请求头）的地方调用。
    pub fn expose(&self) -> &str {
        match self {
            Self::Plain(text) => text,
            Self::Secret(secret) => secret.expose(),
        }
    }
}

/// 一个插件的配置值：键 → 值。
pub type PluginOptions = BTreeMap<String, PluginOption>;

/// 一个已加载的插件：id、名字、目录、数据目录与配置值（ext.md §4.0 的 `PluginSet` 里每个插件一份）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginContext {
    /// `<name>@<marketplace>`。
    pub id: String,
    pub name: String,
    /// 插件目录（执行侧的路径）。
    pub root: String,
    /// 数据目录（执行侧的路径）；没有数据根时为 None（ext.md §4.9）。
    pub data: Option<String>,
    /// 存下的配置值（不含 manifest 的缺省）。
    pub options: PluginOptions,
    /// manifest 声明了 `userConfig`：正文里的 `${user_config.*}` 只在这时展开（ext.md §4.10）。
    pub declares_options: bool,
}

/// `${user_config.KEY}` 在不让用它的地方（ext.md §4.10）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionError {
    /// 引用了敏感键。
    Sensitive(String),
    /// 没有值。
    Missing(String),
}

impl std::fmt::Display for OptionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sensitive(key) => write!(
                formatter,
                "the sensitive plugin option \"{key}\" cannot be used in this field"
            ),
            Self::Missing(key) => write!(formatter, "the plugin option \"{key}\" is not set"),
        }
    }
}

/// `text` 里的每个 `${user_config.KEY}` 换成 `value(KEY)` 的结果；第一个错误时停下。没有结束的 `}` 时后面原样留下。
fn replace_options<E>(
    text: &str,
    mut value: impl FnMut(&str) -> Result<String, E>,
) -> Result<String, E> {
    let marker = format!("${{{OPTION_PREFIX}");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(&marker) {
        let after = &rest[start + marker.len()..];
        let Some(end) = after.find('}') else {
            break;
        };
        out.push_str(&rest[..start]);
        out.push_str(&value(&after[..end])?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// 只能放非敏感值的位置（hook 的 `args`、MCP 的 command / args / cwd / url）：敏感键与没有值的键都是错误。
pub fn expand_plain_options(text: &str, options: &PluginOptions) -> Result<String, OptionError> {
    replace_options(text, |key| match options.get(key) {
        Some(PluginOption::Plain(value)) => Ok(value.clone()),
        Some(PluginOption::Secret(_)) => Err(OptionError::Sensitive(key.to_owned())),
        None => Err(OptionError::Missing(key.to_owned())),
    })
}

/// 能放敏感值的位置（MCP 的 env 与 headers）：没有值的键是错误。
pub fn expand_any_options(text: &str, options: &PluginOptions) -> Result<String, OptionError> {
    replace_options(text, |key| {
        options
            .get(key)
            .map(|value| value.expose().to_owned())
            .ok_or_else(|| OptionError::Missing(key.to_owned()))
    })
}

/// 文字里有没有 `${user_config.…}`（hook 的 shell 形式里写了就是错误，ext.md §4.10）。
pub fn mentions_options(text: &str) -> bool {
    text.contains(&format!("${{{OPTION_PREFIX}"))
}

/// 环境变量名里的键：`[^A-Za-z0-9_]` 换成 `_`、转大写。
fn env_key(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

impl PluginContext {
    /// `${*_PLUGIN_ROOT}` 与 `${*_PLUGIN_DATA}` 换成插件目录与数据目录（两套名字；没有数据目录时 DATA 留原文）。
    pub fn expand_paths(&self, text: &str) -> String {
        let data = self.data.as_deref();
        [
            (vars::CLAUDE_PLUGIN_ROOT, Some(self.root.as_str())),
            (vars::ZCODE_PLUGIN_ROOT, Some(self.root.as_str())),
            (vars::CLAUDE_PLUGIN_DATA, data),
            (vars::ZCODE_PLUGIN_DATA, data),
        ]
        .into_iter()
        .filter_map(|(name, value)| Some((name, value?)))
        .fold(text.to_owned(), |text, (name, value)| {
            text.replace(&vars::placeholder(name), value)
        })
    }

    /// 插件 skill、命令、agent 的正文（ext.md §4.10 的第一行）：先换目录变量，manifest 声明了 `userConfig` 时再换配置值：
    /// 非敏感值经 `escape`（skill 正文转义 `!`，值不能注入 `!` 命令；agent 正文原样），敏感键换成 `sensitive(KEY)` 给的
    /// 占位句，没有值的留原文。
    pub fn expand_content(
        &self,
        text: &str,
        sensitive: &dyn Fn(&str) -> String,
        escape: &dyn Fn(&str) -> String,
    ) -> String {
        let text = self.expand_paths(text);
        if !self.declares_options {
            return text;
        }
        let expanded = replace_options::<std::convert::Infallible>(&text, |key| {
            Ok(match self.options.get(key) {
                Some(PluginOption::Secret(_)) => sensitive(key),
                Some(PluginOption::Plain(value)) => escape(value),
                None => format!("${{{OPTION_PREFIX}{key}}}"),
            })
        });
        match expanded {
            Ok(text) => text,
            Err(never) => match never {},
        }
    }

    /// 给插件 hook 进程的环境变量（ext.md §4.10）：目录、数据目录、id、名字与 `*_PLUGIN_OPTION_<KEY>`（含敏感值）。
    pub fn hook_env(&self) -> Vec<(String, String)> {
        let mut env = vec![
            (vars::CLAUDE_PLUGIN_ROOT.to_owned(), self.root.clone()),
            (vars::ZCODE_PLUGIN_ROOT.to_owned(), self.root.clone()),
            (vars::ZCODE_PLUGIN_ID.to_owned(), self.id.clone()),
            (vars::ZCODE_PLUGIN_NAME.to_owned(), self.name.clone()),
        ];
        if let Some(data) = &self.data {
            env.push((vars::CLAUDE_PLUGIN_DATA.to_owned(), data.clone()));
            env.push((vars::ZCODE_PLUGIN_DATA.to_owned(), data.clone()));
        }
        for (key, value) in &self.options {
            let key = env_key(key);
            for prefix in [vars::CLAUDE_PLUGIN_OPTION, vars::ZCODE_PLUGIN_OPTION] {
                env.push((format!("{prefix}_{key}"), value.expose().to_owned()));
            }
        }
        env
    }
}

/// manifest `userConfig` 里标敏感值的键（ext.md §4.9；插件存储写配置值时按它分私有文件，plugin-store.md §10）。
pub const SENSITIVE_KEY: &str = "sensitive";

/// 插件 id 做目录名（Node `sanitizePluginId`）：`[^a-zA-Z0-9_.@-]` 换成 `-`。数据目录与已安装插件的缺省目录都用它
/// （ext.md §4.2、§4.9）。
pub fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '@' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "plugin_tests.rs"]
mod tests;
