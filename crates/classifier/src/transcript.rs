//! 对话摘录（docs/specs/rust-cli/auto-classifier.md §15.5）：一行一条 JSON，用户的话、AskUserQuestion 的回答、压缩摘要与之前的
//! 调用；助手的文字、工具结果与只读工具的调用不写。最后一行是要判的动作。

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use rust_cli_base::js::json::stringify;
use rust_cli_kernel_api::classifier::{ClassifierInput, EarlierCall, RunTranscript};
use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin, text_of};
use rust_cli_kernel_api::orchestration::ToolRegistry;
use rust_cli_kernel_api::tool_names;
use rust_cli_prompts::auto_mode;
use serde_json::{Map, Value};

/// 历史里不写的调用：只读的工具（读、搜、找工具、读 MCP 资源）。
const UNLISTED: [&str; 7] = [
    tool_names::READ,
    tool_names::GREP,
    tool_names::GLOB,
    tool_names::TOOL_SEARCH,
    tool_names::LIST_MCP_RESOURCES,
    tool_names::READ_MCP_RESOURCE,
    tool_names::READ_MCP_RESOURCE_DIR,
];

/// 用户的话那一行的键。
const USER_KEY: &str = "user";
/// 工具名是这些时（去空白、小写之后）加方括号，不与摘录自己的键混淆。
const RESERVED_KEYS: [&str; 7] = [
    "user",
    "assistant",
    "outcome",
    "id",
    "meta",
    "host_context",
    "host_context_live",
];
/// 内容里这三个标签的 `<` 换成 `[`，不让内容冒充摘录的边界。
const FENCE_TAGS: [&str; 3] = ["transcript", "forwarded_user_turns", "forwarded_turn"];
/// `<`、`>`、`/` 与它们的相似字符。
const OPEN: &str = "<\u{FF1C}\u{FE64}\u{2329}\u{27E8}\u{3008}\u{2039}\u{02C2}\u{1438}\u{276C}\u{276E}\u{2770}\u{29FC}\u{226E}\u{227A}\u{22D6}";
const CLOSE: &str = ">\u{FF1E}\u{FE65}\u{232A}\u{27E9}\u{3009}\u{203A}\u{02C3}\u{1433}\u{276D}\u{276F}\u{2771}\u{29FD}\u{226F}\u{227B}\u{22D7}";
const SLASH: &str = "/\u{FF0F}\u{2215}\u{2044}";
/// JSON 文本里照原样会断行的三个字符，写成 `\uXXXX`。
const LINE_BREAKS: [char; 3] = ['\u{2028}', '\u{2029}', '\u{85}'];
/// 缩进的内容每行前面加的空白。
const INDENT: &str = "  ";

fn fixed(pattern: &str) -> Regex {
    #[expect(clippy::expect_used, reason = "固定的正则")]
    Regex::new(pattern).expect("valid transcript pattern")
}

/// 格式字符与默认可忽略的字符（零宽字符、方向控制等）。
static INVISIBLE: LazyLock<Regex> =
    LazyLock::new(|| fixed(r"[\p{Cf}\p{Default_Ignorable_Code_Point}]"));
/// 三个标签各一个：`<` 后接空白或 `/`、标签名（不分大小写，ASCII 的词边界），到下一个 `>` 为止。
static FENCES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    FENCE_TAGS
        .iter()
        .map(|tag| {
            fixed(&format!(
                r"(?i)[{OPEN}]([\s{SLASH}]*{tag}(?-u:\b)(?:[^{OPEN}{CLOSE}]*[{CLOSE}])?)"
            ))
        })
        .collect()
});
/// 缩进之前统一成 `\n` 的换行写法。
static NEWLINES: LazyLock<Regex> =
    LazyLock::new(|| fixed("\r\n?|[\u{1c}-\u{1e}\u{2028}\u{2029}\u{85}\u{b}\u{c}]"));

/// 去掉看不见的字符，三个标签的 `<` 换成 `[`。
fn neutralize(text: &str) -> String {
    let mut out = INVISIBLE.replace_all(text, "").into_owned();
    for fence in FENCES.iter() {
        out = fence.replace_all(&out, "[$1").into_owned();
    }
    out
}

fn escape_line_breaks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if LINE_BREAKS.contains(&c) {
            out.push_str(&format!("\\u{:04x}", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    out
}

/// 嵌进一行里的多行内容：换行统一、去掉看不见的字符与标签，每行前面加两个空格。
fn indent(text: &str) -> String {
    let unified = NEWLINES.replace_all(text, "\n");
    let neutral = neutralize(&unified);
    format!("{INDENT}{}", neutral.replace('\n', &format!("\n{INDENT}")))
}

/// 一行：`{键: 值}` 的 JSON 文本，去掉看不见的字符与标签，转义断行字符，接 `\n`。
fn line(key: &str, value: Value) -> String {
    let mut object = Map::new();
    object.insert(key.to_owned(), value);
    let json = stringify(&Value::Object(object));
    format!("{}\n", escape_line_breaks(&neutralize(&json)))
}

/// 工具名做键：去掉看不见的字符；与摘录自己的键同名时加方括号。
fn key(name: &str) -> String {
    let name = INVISIBLE.replace_all(name, "").into_owned();
    if RESERVED_KEYS.contains(&name.trim().to_lowercase().as_str()) {
        format!("[{name}]")
    } else {
        name
    }
}

/// 一个调用的那一行；投影是空的时没有这一行。
pub(crate) fn tool_line(name: &str, input: ClassifierInput) -> Option<String> {
    let value = match input {
        ClassifierInput::Skip => return None,
        ClassifierInput::Text(text) => Value::String(text),
        ClassifierInput::Json(value) => value,
    };
    Some(line(&key(name), value))
}

fn user_line(text: String) -> String {
    line(USER_KEY, Value::String(text))
}

/// AskUserQuestion 的回答那一行的文字。
fn answered(answer: &str) -> String {
    format!(
        "[User answered {}]: {}",
        tool_names::ASK_USER_QUESTION,
        indent(answer)
    )
}

/// 历史里一个调用的那一行：只读的工具、找不到的工具与空投影不写。
fn history_call(name: &str, input: &Value, tools: &dyn ToolRegistry) -> Option<String> {
    if UNLISTED.contains(&name) {
        return None;
    }
    let tool = tools.find(name)?;
    tool_line(name, tool.classifier_input(input))
}

/// 渲染好的历史：攒着的行与收好的块。
#[derive(Default)]
struct Blocks {
    blocks: Vec<String>,
    pending: String,
}

impl Blocks {
    fn push_str(&mut self, line: &str) {
        self.pending.push_str(line);
    }

    /// 一个调用的那一行：判过的调用另起一块（先收攒着的行），让下一次请求的前缀与上一次的相同。
    fn call(&mut self, line: Option<String>, classified: bool) {
        let Some(line) = line else { return };
        if classified {
            self.flush();
            self.blocks.push(line);
        } else {
            self.pending.push_str(&line);
        }
    }

    fn flush(&mut self) {
        if !self.pending.is_empty() {
            self.blocks.push(std::mem::take(&mut self.pending));
        }
    }

    fn finish(mut self) -> Vec<String> {
        self.flush();
        self.blocks
    }
}

/// 历史与同一个回复里更早的调用，渲染成摘录里动作之前的几块（auto-classifier.md §15.5、§16.15）：`classified` 是判过的调用，它们
/// 各自成一块；没有一行时为空。
pub(crate) fn history(
    transcript: Option<&RunTranscript>,
    tools: &dyn ToolRegistry,
    classified: &dyn Fn(&ToolUseId) -> bool,
) -> Vec<String> {
    let Some(transcript) = transcript else {
        return Vec::new();
    };
    let mut out = Blocks::default();
    let mut questions = HashSet::new();
    for message in transcript.history.iter() {
        match message {
            Message::User(user) => match &user.origin {
                UserOrigin::Human => {
                    for block in &user.content {
                        if let ContentBlock::Text { text } = block {
                            out.push_str(&user_line(text.clone()));
                        }
                    }
                }
                UserOrigin::ToolResult { .. } => {
                    for block in &user.content {
                        if let ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error: false,
                        } = block
                            && questions.contains(tool_use_id)
                        {
                            let answer = text_of(content);
                            if !answer.is_empty() {
                                out.push_str(&user_line(answered(&answer)));
                            }
                        }
                    }
                }
                UserOrigin::CompactSummary => {
                    let summary = user
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !summary.trim().is_empty() {
                        out.push_str(&user_line(format!(
                            "{}\n{}",
                            auto_mode::compaction_summary(),
                            indent(&summary)
                        )));
                    }
                }
                // 非用户来源的消息（任务通知、跨会话消息、定时任务等）不写（D-C11）。
                UserOrigin::Meta { .. } => {}
            },
            Message::Assistant(assistant) => {
                for block in &assistant.content {
                    if let ContentBlock::ToolUse { id, name, input } = block {
                        if name == tool_names::ASK_USER_QUESTION {
                            questions.insert(id.clone());
                        }
                        out.call(history_call(name, input, tools), classified(id));
                    }
                }
            }
            Message::Attachment(_) | Message::System(_) => {}
        }
    }
    for EarlierCall { id, name, input } in &transcript.earlier {
        out.call(history_call(name, input, tools), classified(id));
    }
    out.finish()
}

#[cfg(test)]
#[path = "transcript_tests.rs"]
mod tests;
