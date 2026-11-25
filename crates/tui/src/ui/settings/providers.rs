//! Models 的子页：你的 provider、新 provider 与一个模型（照抄 Puffin `shell/settings/pages.rs` 的 `model_item`、`provider`、
//! `new_provider`、`provider_form` 与 `model`）。从 `sections.rs` 分出（文件行数的上限）。
//!
//! 页从读 Models 段时读到的 provider 与家族建（runner 按模型目录分好，同 Puffin host 的 `settings.rs` 里的
//! `models`）：目录里的 provider 没有格式、地址与密钥（空的），它的模型都开着、都是内置的；没有模板。改 provider、开关模型、换计划、添加、
//! 删除与测试连接两边的 host 都还没有（tui-kit.md §4.5）：同 Puffin 照样经 runner 写与测，runner 回答这一版还没
//! 有，说的话与回答到的那一帧都同 Puffin。
use std::sync::Arc;

use rust_cli_tui_kit::app::widgets::settings::{self as kit, Applies, Ink, Opt, Part};

use super::page::{Act, Change, Ctx, ItemRow, Page, ProviderEdit, Row, TextRow, View, Write};
use super::tokens::grouped;
use crate::services::FamilyModel;
use crate::ui::i18n::Lang;

/// 写到下一个会话生效的改动（Puffin `next`）。
pub(super) fn next(change: Change) -> Act {
    Act::Write(Write::new(change, Applies::Next))
}

/// provider 的一项改动。
pub(super) fn edit(provider: &str, edit: ProviderEdit) -> Act {
    next(Change::Provider {
        provider: provider.to_owned(),
        edit,
    })
}

/// API 格式：值、名字与它的路径（Puffin `FORMATS`）。
const FORMATS: [(&str, &str, &str); 3] = [
    (
        "openai-chat-completions",
        "Chat completions",
        "/v1/chat/completions",
    ),
    ("openai-responses", "Responses", "/responses"),
    ("anthropic-messages", "Anthropic messages", "/v1/messages"),
];

/// 模型的行（Puffin `model_item` 与 `model_status`）：⏎ 进它的页，空格开关它。
pub(super) fn model_item(l: Lang, provider: &str, m: &FamilyModel, desc: &str) -> Row {
    let status = if m.enabled {
        m.context_window
            .map(|n| match l {
                Lang::En => format!("{} context", kit::tokens_short(n)),
                Lang::Zh => format!("{} 上下文", kit::tokens_short(n)),
            })
            .unwrap_or_default()
    } else {
        l.pick("off", "关").to_owned()
    };
    Row::item(ItemRow {
        key: format!("model:{provider}:{}", m.id),
        name: m.id.clone(),
        toggle: Some(m.enabled),
        status: vec![Part::new(status, Ink::Fg3)],
        desc: desc.to_owned(),
        open: Some(Act::Push(Page::Model {
            provider: provider.to_owned(),
            model: m.id.clone(),
        })),
        flip: Some(next(Change::Model {
            provider: provider.to_owned(),
            model: m.id.clone(),
            enabled: !m.enabled,
        })),
        ..ItemRow::default()
    })
}

/// 你的一个 provider：名字、格式、地址、密钥、开关与它的模型（Puffin `provider`）。
pub(super) fn provider(id: &str, ctx: &Ctx<'_>) -> View {
    let l = ctx.lang;
    let Some(p) = ctx
        .models()
        .and_then(|m| m.providers.iter().find(|p| p.id == id))
    else {
        return View::loading(l, l.pick("Provider", "服务商"));
    };
    let with = |f: fn(String) -> ProviderEdit| {
        let id = id.to_owned();
        move |v: &str| edit(&id, f(v.to_owned()))
    };
    let mut base = TextRow::new(
        "baseUrl",
        l.pick("Base URL", "基础地址"),
        "",
        "",
        Applies::Next,
        with(ProviderEdit::BaseUrl),
    );
    base.placeholder = "https://api.example.com/v1".to_owned();
    let mut key = TextRow::new(
        "apiKey",
        l.pick("API key", "API 密钥"),
        "",
        l.pick(
            "Kept in the same file the desktop app uses.",
            "存在桌面应用用的同一个文件里。",
        ),
        Applies::Next,
        with(ProviderEdit::ApiKey),
    );
    key.secret = true;
    let (format_id, enabled_id) = (id.to_owned(), id.to_owned());
    let mut rows = vec![
        Row::Text(TextRow::new(
            "name",
            name(l),
            p.name.clone(),
            "",
            Applies::Next,
            with(ProviderEdit::Name),
        )),
        Row::Choice {
            key: "format".to_owned(),
            label: l.pick("API format", "API 格式").to_owned(),
            options: FORMATS
                .iter()
                .map(|(v, n, d)| Opt::new(*v, *n).desc(*d))
                .collect(),
            current: String::new(),
            default: String::new(),
            desc: l
                .pick(
                    "The kind of API the service speaks.",
                    "这个服务讲哪种 API。",
                )
                .to_owned(),
            applies: Applies::Next,
            pick: Arc::new(move |v| edit(&format_id, ProviderEdit::Format(v.to_owned()))),
        },
        Row::Text(base),
        Row::Text(key),
        Row::Switch {
            key: "enabled".to_owned(),
            label: l.pick("Enabled", "启用").to_owned(),
            on: true,
            desc: String::new(),
            applies: Applies::Next,
            set: Arc::new(move |on| edit(&enabled_id, ProviderEdit::Enabled(on))),
        },
        Row::Heading(l.pick("Models", "模型").to_owned()),
    ];
    rows.extend(p.models.iter().map(|x| {
        model_item(
            l,
            id,
            x,
            l.pick(
                "⏎ opens its settings and the connection test.",
                "⏎ 打开它的设置与连接测试。",
            ),
        )
    }));
    let delete = l.pick("Delete provider", "删除服务商");
    let deleted = match l {
        Lang::En => format!("Deleted {}.", p.name),
        Lang::Zh => format!("已删除 {}。", p.name),
    };
    rows.push(Row::danger(
        "deleteProvider",
        delete,
        Act::Confirm {
            label: delete.to_owned(),
            note: l
                .pick("This can't be undone.", "删了就恢复不了。")
                .to_owned(),
            next: Box::new(Act::Write(
                Write::new(
                    Change::DeleteProvider {
                        provider: id.to_owned(),
                    },
                    Applies::Next,
                )
                .saying(deleted)
                .going_back(),
            )),
        },
    ));
    if let Some(Row::Action { desc, .. }) = rows.last_mut() {
        *desc = l
            .pick(
                "Its models leave /model. Sessions that use one fail on their next request.",
                "它的模型离开 /model。正在用其中一个的会话在下一次请求时失败。",
            )
            .to_owned();
    }
    View::new(
        vec![p.name.clone()],
        l.pick(
            "Your provider. ZCode sends requests to the address below with your key.",
            "你的服务商。ZCode 带着你的密钥把请求发到下面的地址。",
        ),
        rows,
    )
}

fn name(l: Lang) -> &'static str {
    l.pick("Name", "名字")
}

fn new_provider_crumb(l: Lang) -> &'static str {
    l.pick("New provider", "新服务商")
}

fn custom_provider(l: Lang) -> &'static str {
    l.pick("Custom provider", "自定义服务商")
}

/// 新 provider（Puffin `new_provider`）：两边都没有模板，只有自己描述的那一项。
pub(super) fn new_provider(l: Lang) -> View {
    View::new(
        vec![new_provider_crumb(l).to_owned()],
        l.pick(
            "Start from a service you know, or describe your own.",
            "从你知道的服务开始，或描述你自己的。",
        ),
        vec![Row::plus(
            "custom",
            custom_provider(l),
            l.pick(
                "Name, API format, base URL and key, then its models.",
                "名字、API 格式、地址与密钥，然后是它的模型。",
            ),
            Act::Push(Page::ProviderForm),
        )],
    )
}

/// 给新 provider 起名（Puffin `provider_form`，没有模板时的样子）。
pub(super) fn provider_form(ctx: &Ctx<'_>) -> View {
    let l = ctx.lang;
    let mut field = TextRow::new(
        "np.name",
        name(l),
        ctx.field("np.name", ""),
        "",
        Applies::Form,
        |v| Act::SetForm {
            key: "np.name".to_owned(),
            value: v.to_owned(),
        },
    );
    field.placeholder = l.pick("My provider", "我的服务商").to_owned();
    View::new(
        vec![
            new_provider_crumb(l).to_owned(),
            custom_provider(l).to_owned(),
        ],
        l.pick(
            "Name it; the next page has its address, key and models.",
            "给它起名；下一页是它的地址、密钥与模型。",
        ),
        vec![
            Row::Text(field),
            Row::action(
                "np.add",
                super::sections::add_provider(l),
                "",
                Act::SubmitProvider,
            ),
        ],
    )
}

/// 一个模型：它的 id、上下文与连接测试（Puffin `model` 的内置模型）。先找你的 provider，再找内置的家族，同 Puffin。
pub(super) fn model(pid: &str, mid: &str, ctx: &Ctx<'_>) -> View {
    let owner = ctx.models().and_then(|m| {
        m.providers
            .iter()
            .find(|p| p.id == pid)
            .map(|p| (p.name.as_str(), p.models.as_slice()))
            .or_else(|| {
                m.families
                    .iter()
                    .find(|f| f.provider == pid)
                    .map(|f| (f.name.as_str(), f.models.as_slice()))
            })
    });
    let l = ctx.lang;
    let Some((name, m)) = owner.and_then(|(n, ms)| ms.iter().find(|m| m.id == mid).map(|m| (n, m)))
    else {
        return View::loading(l, mid);
    };
    let mut rows = vec![Row::info(
        l.pick("Model ID", "模型 ID"),
        vec![Part::new(mid, Ink::Fg2)],
    )];
    if let Some(n) = m.context_window {
        rows.push(Row::info(
            l.pick("Context window", "上下文窗口"),
            vec![Part::new(format!("{} tokens", grouped(n)), Ink::Fg2)],
        ));
    }
    rows.push(Row::action(
        "test",
        l.pick("Test connection", "测试连接"),
        l.pick(
            "Sends one short request with your key.",
            "用你的密钥发一个很短的请求。",
        ),
        Act::Test {
            provider: pid.to_owned(),
            model: mid.to_owned(),
        },
    ));
    View::new(
        vec![name.to_owned(), mid.to_owned()],
        l.pick(
            "Built-in model. Its limits come with the provider.",
            "内置的模型。它的上限随服务商。",
        ),
        rows,
    )
}

#[cfg(test)]
#[path = "providers_tests.rs"]
mod tests;
