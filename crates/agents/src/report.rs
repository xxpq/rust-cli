//! 给模型看的子代理文本：system prompt、前台结果、后台启动结果、完成通知（agents.md §2、§4.3、§5）。
//! 文本都在 `prompts/agents/`，这里只拼接。

use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::message::{AssistantMessage, ContentBlock, Usage};
use rust_cli_kernel_api::model::{CacheControl, SystemBlock};
use rust_cli_prompts::agents as texts;
use rust_cli_prompts::texts::{self as context_texts, render};

/// `<total_tokens>` 的预算（padded-countdown 的默认值；与 context 的主线程相同）。
const TOTAL_TOKENS_BUDGET: &str = "15000000";

/// 子代理没有输出时的报告。
pub(crate) const EMPTY_REPORT: &str = "(Subagent completed but returned no output.)";

fn ephemeral(text: String) -> SystemBlock {
    SystemBlock {
        text,
        cache_control: Some(CacheControl {
            kind: "ephemeral".to_owned(),
            ttl: None,
            scope: None,
        }),
    }
}

/// 子代理的 system prompt（`system[1..]`）：身份句；agent 提示词、来自父 agent
/// 的消息说明、Notes 与 `<total_tokens>`，空行连接。
pub(crate) fn system(prompt: &str) -> Vec<SystemBlock> {
    let tokens = render(
        context_texts::total_tokens(),
        &[("tokens", TOTAL_TOKENS_BUDGET)],
    );
    let body = [prompt, texts::authority(), texts::notes(), &tokens]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    vec![
        ephemeral(context_texts::identity().to_owned()),
        ephemeral(body),
    ]
}

/// 子代理最后一条 assistant 消息的文本块（空行连接）。
pub(crate) fn final_text(last: Option<&AssistantMessage>) -> String {
    let Some(last) = last else {
        return String::new();
    };
    last.content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 用量：最后一次响应的输入（含缓存读写）加输出。
pub(crate) fn tokens(usage: Option<Usage>) -> u64 {
    usage.map_or(0, |usage| {
        usage.input_tokens
            + usage.cache_creation_input_tokens
            + usage.cache_read_input_tokens
            + usage.output_tokens
    })
}

/// 子代理一次运行的结果摘要。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Report {
    pub(crate) text: String,
    pub(crate) tokens: u64,
    pub(crate) tool_uses: usize,
    pub(crate) duration_ms: u64,
}

/// 前台结果（A1、A4）：hand-back 说明、每行缩进两格的报告、可选的 agentId 与用量尾注。
pub(crate) fn handback(agent: &str, report: &Report, trailer: bool) -> String {
    let body = if report.text.trim().is_empty() {
        EMPTY_REPORT
    } else {
        report.text.as_str()
    };
    let indented = body
        .split('\n')
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut text = format!("{}\n{indented}", texts::handback());
    if trailer {
        let tokens = report.tokens.to_string();
        let tool_uses = report.tool_uses.to_string();
        let duration = report.duration_ms.to_string();
        text.push('\n');
        text.push_str(&render(
            texts::handback_trailer(),
            &[
                ("id", agent),
                ("tokens", &tokens),
                ("tool_uses", &tool_uses),
                ("duration", &duration),
            ],
        ));
    }
    text
}

/// 后台启动结果（A2，去掉 `output_file` 两行）。
pub(crate) fn launched(agent: &str) -> String {
    render(texts::launched(), &[("id", agent)])
}

/// 谁停了子代理（agents.md §5 的停止者表）：在停止命令进来的地方定下，随停止句柄带到终态，通知只读终态。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum StoppedBy {
    /// 模型调 TaskStop。
    Model,
    /// 用户的停止（`/tasks`、App 的停止）；没有指明停止者时也是它。模型不能再续跑它（agents.md §6）。
    #[default]
    User,
    /// 前台子代理随父工具调用被取消（父回合被停）：不是用户停了这个子代理，模型可以续跑（agents.md §5、§6）。
    CallCancelled,
}

impl StoppedBy {
    /// `<summary>` 里描述之后的部分。模型一方写产品名 `ZCode`（与 Cron 文字相同，agents.md §5）。父调用的取消同用户的那一句
    /// （对照实现的缺省停止者；前台没有通知，不出现在文字里）。
    fn summary(self) -> &'static str {
        match self {
            Self::Model => "was stopped by ZCode",
            Self::User | Self::CallCancelled => "was stopped by user",
        }
    }
}

/// 后台子代理的终态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ending {
    Completed,
    Failed,
    /// 在跑时被停（通知不带用量）。
    Killed(StoppedBy),
    /// 停放时被停（通知带最后的报告与用量）。
    KilledParked(StoppedBy),
}

impl Ending {
    pub(crate) fn status(self) -> BackgroundStatus {
        match self {
            Self::Completed => BackgroundStatus::Completed,
            Self::Failed => BackgroundStatus::Failed,
            Self::Killed(_) | Self::KilledParked(_) => BackgroundStatus::Killed,
        }
    }

    /// 用户停的（`/tasks`、App 的停止）：SendMessage 不续跑它（agents.md §6）。
    pub(crate) fn stopped_by_user(self) -> bool {
        matches!(
            self,
            Self::Killed(StoppedBy::User) | Self::KilledParked(StoppedBy::User)
        )
    }

    /// `<summary>` 里描述之后的部分。
    ///
    /// 修 bug（R-RESUME-NOTICE，agents.md §5）：以前停止者由终态的种类推断——在跑的一律写 TaskStop 的 `by ZCode`、停放的一律写
    /// `by user`，用户在界面上停在跑的子代理也写成 `by ZCode`。现在两种终态都带停止者，这里只照它写。
    fn summary(self, error: Option<&str>) -> String {
        match self {
            Self::Completed => "finished".to_owned(),
            Self::Failed => format!(
                "failed: {}",
                error
                    .filter(|error| !error.is_empty())
                    .unwrap_or("Unknown error")
            ),
            Self::Killed(by) | Self::KilledParked(by) => by.summary().to_owned(),
        }
    }
}

/// `<summary>` 与 `<result>` 里的 `&`、`<`、`>` 转义。
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 完成通知的 `<task-notification>` 信封（不带说明与包装）。`<result>` 只在有最后的文本时出现，
/// `<usage>` 只在完成（与停放时被停止）时出现（A2、A5；agents.md §5、§5.1）。`error` 是失败的原因。
/// `keepalive`：命令保活、停放时送出的通知（agents.md §5.2 第 5 条），`<note>` 换成保活的说明（`notification_note_keepalive`）。
pub(crate) fn envelope(
    agent: &str,
    tool_use: Option<&str>,
    description: &str,
    ending: Ending,
    report: &Report,
    error: Option<&str>,
    keepalive: bool,
) -> String {
    let summary = escape(&format!(
        "Agent \"{description}\" {}",
        ending.summary(error)
    ));
    let mut body = String::new();
    if !report.text.is_empty() {
        body.push('\n');
        body.push_str(&render(
            texts::notification_result(),
            &[("result", &escape(&report.text))],
        ));
    }
    // 用量：完成时有；停放时被停止也有，运行中失败或被停止时没有。
    if matches!(ending, Ending::Completed | Ending::KilledParked(_)) {
        let tokens = report.tokens.to_string();
        let tool_uses = report.tool_uses.to_string();
        let duration = report.duration_ms.to_string();
        body.push('\n');
        body.push_str(&render(
            texts::notification_usage(),
            &[
                ("tokens", &tokens),
                ("tool_uses", &tool_uses),
                ("duration", &duration),
            ],
        ));
    }
    let text = render(
        texts::notification(),
        &[
            ("id", agent),
            ("tool_use", tool_use.unwrap_or_default()),
            ("status", ending.status().as_str()),
            ("summary", &summary),
            (
                "note",
                if keepalive {
                    texts::notification_note_keepalive()
                } else {
                    texts::notification_note()
                },
            ),
            ("body", &body),
        ],
    );
    // 没有工具调用时不写 `<tool-use-id>` 这一行（续跑的通知）。
    match tool_use {
        Some(_) => text,
        None => text.replace("\n<tool-use-id></tool-use-id>", ""),
    }
}

/// 完成通知的全文（A2、A5）：信封加说明与 `<system-reminder>` 包装（运行时由 `routing` 按投递位置
/// 包装信封）。
#[cfg(test)]
pub(crate) fn notification(
    agent: &str,
    tool_use: Option<&str>,
    description: &str,
    ending: Ending,
    report: &Report,
    error: Option<&str>,
) -> String {
    texts::notification_wrap(&envelope(
        agent,
        tool_use,
        description,
        ending,
        report,
        error,
        false,
    ))
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;

#[cfg(test)]
#[cfg(feature = "parity")]
#[rustfmt::skip]
#[path = "report_parity_tests.rs"]
mod parity_tests;
