//! skill 正文的生成（docs/specs/rust-cli/ext.md §2.4 第 1–4 步）：Skill 工具与用户调用（§2.7）共用。
//!
//! 文件 skill：`Base directory for this skill: <目录>` 加正文，参数替换、变量、`!` 内联命令；内置 skill 是
//! 生成的提示（tools-more.md §17.1）。

use rust_cli_base::names::vars;
use rust_cli_kernel_api::tool::ToolUseContext;

use super::inline::Runner;
use super::{Skill, SkillCatalog, SkillSource, args, builtin};

/// 变量替换（ext.md §2.4 第 3 步）：两套名字都认。没有基础目录的命令（§2.10）不替换 skill 目录的两个变量。
fn variables(text: String, skill: &Skill, context: &ToolUseContext) -> String {
    // Windows 风格路径的 `\` 换成 `/`
    let dir = skill.dir.as_deref().map(|dir| dir.replace('\\', "/"));
    let root = context.workspace.root.as_str();
    let session = context.session.as_str();
    let skill_dir = dir
        .as_deref()
        .into_iter()
        .flat_map(|dir| [(vars::CLAUDE_SKILL_DIR, dir), (vars::ZCODE_SKILL_DIR, dir)]);
    skill_dir
        .chain([
            (vars::CLAUDE_PROJECT_DIR, root),
            (vars::ZCODE_PROJECT_DIR, root),
            (vars::CLAUDE_SESSION_ID, session),
            (vars::ZCODE_SESSION_ID, session),
        ])
        .fold(text, |text, (name, value)| {
            text.replace(&vars::placeholder(name), value)
        })
}

/// 敏感配置值在正文里的占位句（ext.md §4.10）。
pub(crate) fn sensitive_option(key: &str) -> String {
    rust_cli_prompts::texts::render(
        rust_cli_prompts::texts::plugin_sensitive_option(),
        &[("key", key)],
    )
}

/// skill 的正文；`!` 命令被拒绝或失败时返回错误文本（`Shell command … failed for pattern …`）。
pub(crate) async fn render(
    catalog: &SkillCatalog,
    runner: Option<&Runner>,
    skill: &Skill,
    args: &str,
    context: &ToolUseContext,
) -> Result<String, String> {
    if skill.source == SkillSource::Builtin {
        if let Some(body) = builtin::prompt_command(catalog, skill, context).await {
            return body;
        }
        // 在 git 仓库里的 `/security-review`：正文的 `!` 命令照 skill 的规则执行。
        if skill.name == builtin::SECURITY_REVIEW {
            let content = rust_cli_prompts::commands::security_review().to_owned();
            return match runner {
                Some(runner) => runner.expand(content, &skill.allowed_tools, context).await,
                None => Ok(content),
            };
        }
        return Ok(builtin::prompt(catalog, context, args).await);
    }
    let content = match &skill.dir {
        Some(dir) => format!("Base directory for this skill: {dir}\n\n{}", skill.body),
        None => skill.body.clone(),
    };
    let content = args::substitute(&content, args, &skill.arg_names);
    // 插件的 skill 与命令：插件目录、数据目录与配置值（ext.md §4.10）；在 skill 目录与会话 ID 之前（参照的顺序）。
    let content = match &skill.source {
        SkillSource::Plugin { plugin } => match catalog.plugins().find(plugin) {
            Some(plugin) => {
                plugin
                    .context
                    .expand_content(&content, &sensitive_option, &args::escape_bang)
            }
            None => content,
        },
        _ => content,
    };
    let content = variables(content, skill, context);
    match runner {
        Some(runner) => runner.expand(content, &skill.allowed_tools, context).await,
        None => Ok(content),
    }
}
