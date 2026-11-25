//! 会话的附加工作目录（docs/specs/rust-cli/session-dirs.md）：要加的目录怎么检查、算不算已在工作目录里、会话起始时收哪些，
//! 以及 `/add-dir` 各种结局的文字。入口（`--add-dir`）、runtime（`/add-dir` 的路径检查）与会话 actor（判定与记录）共用这一份。

use serde::{Deserialize, Serialize};

use crate::env::{EnvPath, WorkspaceRef};
use crate::env_files::{FileKind, PathStyle, StatFile};
use crate::machine::Files;
use crate::user_command::{bold, dim};

/// 一个目录已经在会话的工作目录里（session-dirs.md §5.3 的四种文字由它定）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Covered {
    /// 包含它的那一个工作目录：工作区根或某个附加目录。
    pub by: EnvPath,
    /// 就是它（不是在它里面）。
    pub exact: bool,
    /// `by` 是工作区根。
    pub root: bool,
}

/// `/add-dir` 在会话里的结局（`CommandResult::DirectoryAddition`，session-dirs.md §3.2）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Addition {
    /// 加进会话了；`remembered` 是确认框里选了「记住」时有没有存进本机项目规则集（permission-updates.md §6.1）。
    Added {
        #[serde(default)]
        remembered: Remembered,
    },
    Covered(Covered),
}

/// `/add-dir` 的「记住」（docs/specs/rust-cli/permission-updates.md §6.1）：没选、存好了，或没存上（仍加进会话，`reason` 写进
/// 文字）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Remembered {
    #[default]
    No,
    Saved,
    Failed {
        reason: String,
    },
}

/// 要加的目录为什么用不了（session-dirs.md §5.3 的三种）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unusable {
    /// 参数是空的。
    Empty,
    /// 不存在或读不到。
    Missing { absolute: String },
    /// 是文件不是目录。
    NotDirectory { parent: String },
}

/// `path` 在哪个工作目录里：先看工作区根，再按顺序看附加目录；都不在时为 None。路径风格按工作区根推断（Windows 上不分
/// 大小写，kernel-api `env_path` 的键）。
pub fn covering(root: &EnvPath, additional: &[EnvPath], path: &str) -> Option<Covered> {
    let style = PathStyle::infer(root.as_str());
    std::iter::once((root, true))
        .chain(additional.iter().map(|dir| (dir, false)))
        .find(|(dir, _)| style.is_within(path, dir.as_str()))
        .map(|(dir, is_root)| Covered {
            by: dir.clone(),
            exact: style.same(path, dir.as_str()),
            root: is_root,
        })
}

/// 会话起始的附加目录（session-dirs.md §3.1）：按顺序收 `entries`；不是绝对路径的（基准说不清）、已在工作区根或前面收下的
/// 目录里的不收。不 `stat`：调用方给的是它自己的事实。
pub fn admit(root: &EnvPath, entries: impl IntoIterator<Item = EnvPath>) -> Vec<EnvPath> {
    let style = PathStyle::infer(root.as_str());
    let mut admitted: Vec<EnvPath> = Vec::new();
    for entry in entries {
        if style.is_absolute(entry.as_str()) && covering(root, &admitted, entry.as_str()).is_none()
        {
            admitted.push(entry);
        }
    }
    admitted
}

/// 解析并检查一个要加的目录：`~` 与相对路径按 `base` 解析，`stat` 是目录时给出绝对路径。读不到（不只是不存在）也算
/// 不存在。`input` 原样用（`/add-dir` 的参数由调用方先去掉首尾空白）。
pub async fn check(
    (files, workspace): (&dyn Files, &WorkspaceRef),
    style: PathStyle,
    (base, home): (&str, Option<&str>),
    input: &str,
) -> Result<String, Unusable> {
    if input.is_empty() {
        return Err(Unusable::Empty);
    }
    let absolute = style.resolve(base, input, home);
    let request = StatFile {
        path: EnvPath::new(&absolute),
    };
    match files.stat(workspace, request).await {
        Ok(Some(stat)) if stat.kind == FileKind::Directory => Ok(absolute),
        Ok(Some(_)) => Err(Unusable::NotDirectory {
            parent: style.dirname(&absolute),
        }),
        Ok(None) | Err(_) => Err(Unusable::Missing { absolute }),
    }
}

/// 用不了的文字（session-dirs.md §5.3）。`emphasize` 给路径加样式：记进对话的是 [`bold`]，给入口警告的是原样。
pub fn unusable_text(input: &str, unusable: &Unusable, emphasize: fn(&str) -> String) -> String {
    match unusable {
        Unusable::Empty => "Please provide a directory path.".to_owned(),
        Unusable::Missing { absolute } => format!("Path {} was not found.", emphasize(absolute)),
        Unusable::NotDirectory { parent } => format!(
            "{} is not a directory. Did you mean to add the parent directory {}?",
            emphasize(input),
            emphasize(parent)
        ),
    }
}

/// 已在工作目录里的文字（session-dirs.md §5.3）。
pub fn covered_text(input: &str, covered: &Covered, emphasize: fn(&str) -> String) -> String {
    let shown = emphasize(input);
    match (covered.exact, covered.root) {
        (true, true) => format!("{shown} is already the current working directory."),
        (true, false) => format!("{shown} is already added as a working directory."),
        (false, root) => format!(
            "{shown} is already accessible within {} {}.",
            if root {
                "the current working directory"
            } else {
                "the additional working directory"
            },
            emphasize(covered.by.as_str())
        ),
    }
}

/// 加进去了，记进对话的那一份（session-dirs.md §5.3；记住的两种见 permission-updates.md §6.1）：路径粗体，后缀
/// ` · /permissions to manage` 是 dim。
pub fn added_record(directory: &str, remembered: &Remembered) -> String {
    let shown = bold(directory);
    let said = match remembered {
        Remembered::No => format!("Added {shown} as a working directory for this session"),
        Remembered::Saved => {
            format!("Added {shown} as a working directory and saved to local settings")
        }
        Remembered::Failed { reason } => format!(
            "Added {shown} as a working directory. Failed to save to local settings: {reason}"
        ),
    };
    format!("{said} {}", dim("· /permissions to manage"))
}

/// 确认框里选了「不加」或按了 esc：记进对话的那一份，路径粗体。
pub fn declined_record(directory: &str) -> String {
    format!("Did not add {} as a working directory.", bold(directory))
}

/// 没有参数时的用法（不开输入框，session-dirs.md D2）。
pub const USAGE: &str = "Usage: /add-dir <path>";

/// 命令名（不带 `/`）：TUI 的命令表与本地命令的记录。
pub const COMMAND: &str = "add-dir";

#[cfg(test)]
#[path = "working_dirs_tests.rs"]
mod tests;
