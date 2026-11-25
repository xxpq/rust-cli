//! MCP 服务器的询问在 TUI 里（docs/specs/rust-cli/tui-kit.md §4.3「MCP 的询问」，ext.md §3.13、§3.14）：
//!
//! - 表单照提问画（kit 的 `AskBody::Question`）：一个字段一页（布尔是「是 / 否」、单选与多选是选项、文本与数字在真的输入框里打），
//!   最后一页是「提交 / 拒绝」两个按钮。输入时与提交时用 kernel-api 的同一份校验，不合格时停在那一页说原因；
//! - 网址照提问画一页：「在浏览器里打开 / 拒绝」，同意时先回 `accept` 再打开；
//! - esc 是取消：只关这一问，回合继续；
//! - sampling 照权限询问画：允许一次、本会话一直允许、拒绝。
use std::collections::BTreeMap;

use rust_cli_kernel_api::interaction::Answer;
use rust_cli_kernel_api::mcp_ask::{
    Elicitation, ElicitationAnswer, ElicitationKind, FieldKind, FormField, Sampling,
    SamplingAnswer, SamplingRole, accept,
};
use rust_cli_tui_kit::app::widgets::asks::data::{
    AskField, AskOption, AskOptionKind, AskPreview, QuestionOption, QuestionPage,
};
use serde_json::{Map, Value};

use super::i18n::Lang;
use super::interaction_ask::{ALLOW_ONCE, DENY};

/// 询问的「工具名」（kit 的 `Ask.tool_name`；v4 的 CLI 给同一种交互的名字）。
pub const ELICITATION: &str = "McpElicitation";
pub const SAMPLING: &str = "McpSampling";
/// sampling 的「本会话一直允许」（kit 的 `AllowSession`）。
pub const ALLOW_SESSION: &str = "allowSession";
/// 最后一页与网址那一页的两个回答。
const ACCEPT: &str = "accept";
const DECLINE: &str = "decline";
/// 布尔字段的两个回答（值是 kernel-api 校验认的文字）。
const YES: &str = "true";
const NO: &str = "false";

/// 页上的字（tui-kit.md §4.3「MCP 的询问」）。
fn say(lang: Lang, en: &'static str, zh: &'static str) -> String {
    match lang {
        Lang::En => en,
        Lang::Zh => zh,
    }
    .to_owned()
}

fn option(value: &str, label: String) -> QuestionOption {
    QuestionOption {
        value: value.to_owned(),
        label,
        ..QuestionOption::default()
    }
}

/// 每一页的题目：服务器名与它的话（题头 `MCP server “x” requests your input`），字段有说明时接在后面。
fn question(lang: Lang, elicitation: &Elicitation, detail: Option<&str>) -> String {
    let head = match (lang, &elicitation.kind) {
        (Lang::En, ElicitationKind::Form { .. }) => {
            format!("MCP server “{}” requests your input", elicitation.server)
        }
        (Lang::En, ElicitationKind::Url { .. }) => {
            format!("MCP server “{}” wants to open a URL", elicitation.server)
        }
        (Lang::Zh, ElicitationKind::Form { .. }) => {
            format!("MCP 服务器「{}」要你填一些信息", elicitation.server)
        }
        (Lang::Zh, ElicitationKind::Url { .. }) => {
            format!("MCP 服务器「{}」想打开一个网址", elicitation.server)
        }
    };
    let mut text = format!("{head}\n\n{}", elicitation.message);
    if let Some(detail) = detail.filter(|detail| !detail.is_empty()) {
        text.push_str("\n\n");
        text.push_str(detail);
    }
    text
}

/// 一个字段的页：页名是它的名字（必填的带 `*`）。
fn field_page(lang: Lang, elicitation: &Elicitation, field: &FormField) -> QuestionPage {
    let header = if field.required {
        format!("{} *", field.label())
    } else {
        field.label().to_owned()
    };
    let (options, multi) = match &field.kind {
        FieldKind::Boolean { .. } => (
            vec![
                option(YES, say(lang, "Yes", "是")),
                option(NO, say(lang, "No", "否")),
            ],
            false,
        ),
        FieldKind::Choice { options, .. } => (
            options
                .iter()
                .map(|o| option(&o.value, o.label.clone()))
                .collect(),
            false,
        ),
        FieldKind::Choices { options, .. } => (
            options
                .iter()
                .map(|o| option(&o.value, o.label.clone()))
                .collect(),
            true,
        ),
        FieldKind::Text { .. } | FieldKind::Number { .. } => (Vec::new(), false),
    };
    QuestionPage {
        header,
        question: question(lang, elicitation, field.description.as_deref()),
        multi,
        options,
    }
}

/// elicitation 的页：表单每个字段一页、最后一页提交或拒绝；网址一页。
pub fn pages(lang: Lang, elicitation: &Elicitation) -> Vec<QuestionPage> {
    let mut pages: Vec<QuestionPage> = match &elicitation.kind {
        ElicitationKind::Form { fields } => fields
            .iter()
            .map(|field| field_page(lang, elicitation, field))
            .collect(),
        ElicitationKind::Url { url } => {
            return vec![QuestionPage {
                header: say(lang, "Open URL", "打开网址"),
                question: question(lang, elicitation, Some(url)),
                multi: false,
                options: vec![
                    option(ACCEPT, say(lang, "Open in browser", "在浏览器里打开")),
                    option(DECLINE, say(lang, "Decline", "拒绝")),
                ],
            }];
        }
    };
    pages.push(QuestionPage {
        header: say(lang, "Submit", "提交"),
        question: question(lang, elicitation, None),
        multi: false,
        options: vec![
            option(ACCEPT, say(lang, "Accept", "提交")),
            option(DECLINE, say(lang, "Decline", "拒绝")),
        ],
    });
    pages
}

/// 打开时的初值（schema 的 `default`）：选项字段选中、文本与数字字段写进这一页自己的回答。
pub fn initial(
    elicitation: &Elicitation,
) -> (BTreeMap<usize, Vec<usize>>, BTreeMap<usize, String>) {
    let mut picks = BTreeMap::new();
    let mut own = BTreeMap::new();
    let ElicitationKind::Form { fields } = &elicitation.kind else {
        return (picks, own);
    };
    for (page, field) in fields.iter().enumerate() {
        let Some(value) = field.initial() else {
            continue;
        };
        match &field.kind {
            FieldKind::Boolean { .. } => {
                picks.insert(page, vec![usize::from(value == Value::Bool(false))]);
            }
            FieldKind::Choice { options, .. } | FieldKind::Choices { options, .. } => {
                let chosen: Vec<usize> = options
                    .iter()
                    .enumerate()
                    .filter(|(_, o)| match &value {
                        Value::String(text) => *text == o.value,
                        Value::Array(items) => {
                            items.iter().any(|item| item.as_str() == Some(&o.value))
                        }
                        _ => false,
                    })
                    .map(|(i, _)| i)
                    .collect();
                picks.insert(page, chosen);
            }
            FieldKind::Text { .. } | FieldKind::Number { .. } => {
                own.insert(
                    page,
                    value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned),
                );
            }
        }
    }
    (picks, own)
}

/// 文本、数字字段那一页打完 ⏎ 时：不合格的原因（空的必填字段在提交时才说）。不是表单的字段页时没有。
pub fn check_typed(elicitation: &Elicitation, page: usize, typed: &str) -> Option<String> {
    let ElicitationKind::Form { fields } = &elicitation.kind else {
        return None;
    };
    let field = fields.get(page)?;
    if typed.is_empty() {
        return None;
    }
    field.check(&Value::String(typed.to_owned())).err()
}

/// 一个字段的值：选项字段是选中的选项值（布尔写成 `true` / `false` 的文字，多选是数组），文本与数字是这一页自己的回答。
fn value_of(
    field: &FormField,
    page: &QuestionPage,
    picks: &[usize],
    own: Option<&String>,
) -> Option<Value> {
    let values = picks
        .iter()
        .filter_map(|i| page.options.get(*i))
        .map(|o| Value::String(o.value.clone()));
    match &field.kind {
        FieldKind::Choices { .. } => {
            let values: Vec<Value> = values.collect();
            (!values.is_empty()).then_some(Value::Array(values))
        }
        FieldKind::Boolean { .. } | FieldKind::Choice { .. } => values.into_iter().next(),
        FieldKind::Text { .. } | FieldKind::Number { .. } => own.cloned().map(Value::String),
    }
}

/// 最后一页（或网址那一页）选了之后的回答。表单提交时整份校验：不合格时是那个字段的页与原因（停在那一页）。网址的同意带要打开的
/// 网址。
pub fn submit(
    elicitation: &Elicitation,
    pages: &[QuestionPage],
    picks: &BTreeMap<usize, Vec<usize>>,
    own: &BTreeMap<usize, String>,
) -> Result<(Answer, Option<String>), (usize, String)> {
    let last = pages.len().saturating_sub(1);
    let chosen = picks
        .get(&last)
        .and_then(|picked| picked.first())
        .and_then(|i| pages.get(last)?.options.get(*i))
        .map(|o| o.value.as_str());
    if chosen != Some(ACCEPT) {
        return Ok((Answer::McpElicitation(ElicitationAnswer::Decline), None));
    }
    let fields = match &elicitation.kind {
        ElicitationKind::Url { url } => {
            let accepted = ElicitationAnswer::Accept {
                content: Map::new(),
            };
            return Ok((Answer::McpElicitation(accepted), Some(url.clone())));
        }
        ElicitationKind::Form { fields } => fields,
    };
    let mut content = Map::new();
    for (page, field) in fields.iter().enumerate() {
        let Some(shown) = pages.get(page) else {
            continue;
        };
        let picked = picks.get(&page).map_or(&[][..], Vec::as_slice);
        if let Some(value) = value_of(field, shown, picked, own.get(&page)) {
            content.insert(field.name.clone(), value);
        }
    }
    let content = accept(fields, &content).map_err(|error| {
        let page = fields
            .iter()
            .position(|field| field.name == error.field)
            .unwrap_or_default();
        (page, error.message)
    })?;
    Ok((
        Answer::McpElicitation(ElicitationAnswer::Accept { content }),
        None,
    ))
}

/// sampling 的预览：服务器、系统提示、每条消息与输出上限，每个一行（kit 的 `Fields`）。
pub fn sampling_preview(lang: Lang, sampling: &Sampling) -> AskPreview {
    let field = |key: String, value: String| AskField { key, value };
    let mut fields = vec![field(
        say(lang, "server", "服务器"),
        sampling.server.clone(),
    )];
    if let Some(system) = &sampling.system {
        fields.push(field(say(lang, "system", "系统提示"), system.clone()));
    }
    for message in &sampling.messages {
        let role = match message.role {
            SamplingRole::User => say(lang, "user", "用户"),
            SamplingRole::Assistant => say(lang, "assistant", "助手"),
        };
        fields.push(field(role, message.text.clone()));
    }
    fields.push(field(
        say(lang, "max tokens", "输出上限"),
        sampling.max_tokens.to_string(),
    ));
    AskPreview::Fields { fields }
}

/// sampling 的回答：允许一次、本会话一直允许、拒绝。
pub fn sampling_options() -> Vec<AskOption> {
    let option = |id: &str, kind| AskOption {
        id: id.to_owned(),
        kind,
        rules: Vec::new(),
    };
    vec![
        option(ALLOW_ONCE, AskOptionKind::AllowOnce),
        option(ALLOW_SESSION, AskOptionKind::AllowSession),
        option(DENY, AskOptionKind::Deny),
    ]
}

/// sampling 选项 → 回答；不认识的选项为 None。
pub fn sampling_answer(option: &str) -> Option<Answer> {
    Some(Answer::McpSampling(match option {
        ALLOW_ONCE => SamplingAnswer::SamplingAllowed { session: false },
        ALLOW_SESSION => SamplingAnswer::SamplingAllowed { session: true },
        DENY => SamplingAnswer::SamplingDenied,
        _ => return None,
    }))
}

#[cfg(test)]
#[path = "interaction_form_tests.rs"]
mod tests;
