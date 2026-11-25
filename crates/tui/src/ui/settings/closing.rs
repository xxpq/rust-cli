//! 关上设置页时说改了什么（docs/specs/rust-cli/tui-kit.md §4.5「关上时说改了什么」，用户决定 2026-11-12）：
//! 打开时与每段读到时记下值行的原值，关上时与现在的值比，变了的每项一行。
//! 值只来自存储读回的数据与 App 的偏好（行是它们的纯函数），这里不另存设置的值。
use std::collections::BTreeMap;

use rust_cli_tui_kit::app::widgets::settings::SECTIONS;

use super::page::{self, Ctx, Row};
use crate::ui::i18n::Lang;
use crate::ui::i18n_settings::words;

/// 一个值行：原始的值（比较用，不随语言）与说的时候要的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Value {
    raw: String,
    label: String,
    shown: Shown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Shown {
    Switch(bool),
    Text(String),
}

/// 各段顶层的值行（开关、选择、文字、模型选择器），键是 `段/行`，按段与行的次序。只读的三段没有值行。
pub fn values(ctx: &Ctx<'_>) -> Vec<(String, Value)> {
    let w = words(ctx.lang);
    SECTIONS
        .iter()
        .flat_map(|s| {
            page::top(s.id, ctx)
                .into_iter()
                .filter_map(move |r| value(&r, w).map(|v| (format!("{}/{}", s.id, r.id()), v)))
        })
        .collect()
}

fn value(r: &Row, w: &rust_cli_tui_kit::app::widgets::settings::SettingsWords) -> Option<Value> {
    let (raw, label, shown) = match r {
        Row::Switch { label, on, .. } => (on.to_string(), label, Shown::Switch(*on)),
        Row::Choice {
            label,
            options,
            current,
            ..
        } => {
            let name = options
                .iter()
                .find(|o| o.value == *current)
                .map_or_else(|| current.clone(), |o| o.label.clone());
            (current.clone(), label, Shown::Text(name))
        }
        Row::Text(t) => {
            let shown = if t.value.is_empty() {
                if t.empty.is_empty() {
                    w.not_set.to_owned()
                } else {
                    t.empty.clone()
                }
            } else {
                t.value.clone()
            };
            (t.value.clone(), &t.label, Shown::Text(shown))
        }
        Row::Picker { label, value, .. } => match value {
            Some(p) => {
                let shown = if p.effort.is_empty() {
                    p.model.clone()
                } else {
                    format!("{} · {}", p.model, p.effort)
                };
                (
                    format!("{}\n{}\n{}", p.provider, p.model, p.effort),
                    label,
                    Shown::Text(shown),
                )
            }
            None => (String::new(), label, Shown::Text(w.not_set.to_owned())),
        },
        _ => return None,
    };
    Some(Value {
        raw,
        label: label.clone(),
        shown,
    })
}

/// 记下还没记的原值（打开时与每段读到时）：已经记了的不动，所以比的总是打开以来第一次看见的值。
pub fn note(first: &mut BTreeMap<String, String>, now: Vec<(String, Value)>) {
    for (key, v) in now {
        first.entry(key).or_insert(v.raw);
    }
}

/// 关上时说的：现在的值与原值不同的每项一行（没有原值的不算）。
pub fn said(
    lang: Lang,
    first: &BTreeMap<String, String>,
    now: Vec<(String, Value)>,
) -> Vec<String> {
    now.into_iter()
        .filter(|(key, v)| first.get(key).is_some_and(|raw| *raw != v.raw))
        .map(|(_, v)| line(lang, &v))
        .collect()
}

/// `Set theme to Dark` / `Disabled ← opens the task list`；中文 `已将主题设为深色` / `已关闭 ← 打开任务列表`。
fn line(lang: Lang, v: &Value) -> String {
    match (lang, &v.shown) {
        (Lang::En, Shown::Switch(on)) => {
            format!("{} {}", if *on { "Enabled" } else { "Disabled" }, v.label)
        }
        (Lang::En, Shown::Text(shown)) => format!("Set {} to {shown}", lower_first(&v.label)),
        (Lang::Zh, Shown::Switch(on)) => {
            format!("已{} {}", if *on { "开启" } else { "关闭" }, v.label)
        }
        (Lang::Zh, Shown::Text(shown)) => {
            // 值以英文开头时隔一格（`已将主题设为 Puffin`），中文紧贴（`已将明暗设为深色`）。
            let gap = if shown.starts_with(|c: char| c.is_ascii()) {
                " "
            } else {
                ""
            };
            format!("已将{}设为{gap}{shown}", v.label)
        }
    }
}

/// 行名的首字母小写（`Set theme to …`）；第二个字母也是大写（缩写）时不动。
fn lower_first(label: &str) -> String {
    let mut chars = label.chars();
    match (chars.next(), chars.next()) {
        (Some(a), Some(b)) if a.is_ascii_uppercase() && !b.is_ascii_uppercase() => {
            let mut out = a.to_ascii_lowercase().to_string();
            out.push_str(&label[a.len_utf8()..]);
            out
        }
        _ => label.to_owned(),
    }
}
