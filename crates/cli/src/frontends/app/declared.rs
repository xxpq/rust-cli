//! App 的 Host 声明的前端能力（docs/specs/rust-cli/context.md §15.5）：`app-server --frontend <名字,…>` 与 Gen UI 的输出根目录。
//!
//! 只声明能力（闭集的名字），不带给模型的文字：文字在 App 前端层。这里把声明分成内核认识的两项（`FrontendTraits`）与 App 的
//! 渲染能力（[`Renders`]）。一个进程只接一个 Host，能力在进程寿命内不变，所以是启动参数；出现一个进程服务能力不同的前端时
//! 挪进握手（§15.5 末段）。

use std::path::Path;

use rust_cli_base::secret::EnvSnapshot;
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::session_prompt::FrontendTraits;

use super::Renders;

/// 启动参数的名字。
pub(crate) const FRONTEND_FLAG: &str = "--frontend";
/// Gen UI 的输出根目录：Host 给 Node agent 与 rust 的同一个变量（`packages/shared/src/node/genUiPaths.ts` 的
/// `GEN_UI_OUTPUT_ROOT_ENV`）。只在入口层读：内核与它下面的层不认识 Host 的变量（app-protocol.md §9.2 的词汇检查），所以不放进
/// base 的 `names::env`。
pub(crate) const GEN_UI_OUTPUT_ROOT: &str = "ZCODE_GEN_UI_OUTPUT_ROOT";
/// 名字之间的分隔。
const SEPARATOR: char = ',';

/// 一项声明的能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Declared {
    ShellEscape,
    TypedMentions,
    CodeComments,
    FileLinks,
    GenUi,
}

/// 闭集：启动参数里的名字与能力。
const NAMES: [(&str, Declared); 5] = [
    ("shell-escape", Declared::ShellEscape),
    ("typed-mentions", Declared::TypedMentions),
    ("code-comments", Declared::CodeComments),
    ("file-links", Declared::FileLinks),
    ("gen-ui", Declared::GenUi),
];

/// `--frontend` 的值：逗号分隔的名字（两边的空白不算）。不认识的名字报错，写出允许的名字（同 `--protocol`）。
pub(crate) fn parse(value: &str) -> Result<Vec<Declared>, String> {
    value
        .split(SEPARATOR)
        .map(str::trim)
        .map(|name| {
            NAMES
                .iter()
                .find(|(known, _)| *known == name)
                .map(|(_, declared)| *declared)
                .ok_or_else(|| {
                    let allowed: Vec<&str> = NAMES.iter().map(|(known, _)| *known).collect();
                    format!(
                        "error: option '{FRONTEND_FLAG} <traits>' argument '{name}' is invalid. Allowed choices are {}.",
                        allowed.join(", ")
                    )
                })
        })
        .collect()
}

/// 换成内核认识的能力与 App 的渲染能力。声明了 `gen-ui` 时输出根目录取 [`GEN_UI_OUTPUT_ROOT`]；没有或不是绝对路径是 Host 的错，
/// 启动失败（不降级成不支持：那样模型拿不到目录，App 却以为能渲染）。
pub(super) fn declare(
    declared: &[Declared],
    env: &EnvSnapshot,
) -> Result<(FrontendTraits, Renders), String> {
    let has = |wanted: Declared| declared.contains(&wanted);
    let gen_ui = if has(Declared::GenUi) {
        match env.text(GEN_UI_OUTPUT_ROOT) {
            Some(root) if Path::new(root).is_absolute() => Some(EnvPath::new(root)),
            _ => {
                return Err(format!(
                    "error: '{FRONTEND_FLAG} gen-ui' needs an absolute directory in {GEN_UI_OUTPUT_ROOT}"
                ));
            }
        }
    } else {
        None
    };
    Ok((
        FrontendTraits {
            shell_escape: has(Declared::ShellEscape),
            typed_mentions: has(Declared::TypedMentions),
        },
        Renders {
            code_comments: has(Declared::CodeComments),
            file_links: has(Declared::FileLinks),
            gen_ui,
        },
    ))
}

#[cfg(test)]
#[path = "declared_tests.rs"]
mod tests;
