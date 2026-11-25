//! 分类器用哪个模型（docs/specs/rust-cli/auto-classifier.md §15.6.1）：主模型是 Anthropic 的模型时用 Sonnet 5（同一个 provider），
//! 第一次在一个 provider 上用它时算探测；探测失败（不是鉴权失败）之后这个 provider 退回主模型。其余主模型就是分类器的模型。

use std::collections::HashMap;
use std::sync::Mutex;

use rust_cli_base::models;
use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::model::{ModelSelection, ThinkingConfig};

use crate::limits::ALWAYS_THINKING_EXTRA_TOKENS;

/// 一个 provider 上探测的结论。没有记录就是还没探测。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Probe {
    Confirmed,
    Demoted,
}

/// 这一次用的模型。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Choice {
    pub(crate) selection: ModelSelection,
    /// 这一次是不是这个 provider 上的探测（结论回来之后记下）。
    pub(crate) probing: bool,
}

/// 各 provider 的探测结论：只在内存，只从“还没探测”变到确认或退回。
#[derive(Default)]
pub(crate) struct Probes(Mutex<HashMap<String, Probe>>);

impl Probes {
    pub(crate) fn choose(&self, main: &ModelSelection) -> Choice {
        let probe = self.0.lock_unpoisoned().get(&main.provider).copied();
        match sonnet(main) {
            Some(selection) if probe != Some(Probe::Demoted) => Choice {
                selection,
                probing: probe.is_none(),
            },
            _ => Choice {
                selection: main.clone(),
                probing: false,
            },
        }
    }

    /// 记下探测的结论；已经有结论时不改（并发的几次探测里先回来的落下）。
    pub(crate) fn settle(&self, provider: &str, probe: Probe) {
        self.0
            .lock_unpoisoned()
            .entry(provider.to_owned())
            .or_insert(probe);
    }
}

/// 主模型是 Anthropic 的模型、不是那两个保留主模型的版本时：同一个 provider 与账号连接上的 Sonnet 5（主模型带 1M 档位时也带）。
fn sonnet(main: &ModelSelection) -> Option<ModelSelection> {
    let model = main.model.as_str();
    let base = model
        .strip_suffix(models::ONE_MILLION_SUFFIX)
        .unwrap_or(model);
    if !models::is_anthropic(base)
        || models::CLASSIFIER_KEEPS_MAIN
            .iter()
            .any(|kept| base.starts_with(kept))
    {
        return None;
    }
    let suffix = if base.len() == model.len() {
        ""
    } else {
        models::ONE_MILLION_SUFFIX
    };
    Some(ModelSelection {
        connection: main.connection.clone(),
        ..ModelSelection::new(
            main.provider.clone(),
            format!("{}{suffix}", models::SONNET_5),
        )
    })
}

/// 分类器请求的 thinking 与另加的输出余量：能关的关掉；不能关的模型不写 thinking，输出上限另加余量。
pub(crate) fn thinking(model: &str) -> (Option<ThinkingConfig>, u32) {
    let base = model
        .strip_suffix(models::ONE_MILLION_SUFFIX)
        .unwrap_or(model);
    if models::REJECTS_DISABLED_THINKING.contains(&base) {
        (None, ALWAYS_THINKING_EXTRA_TOKENS)
    } else {
        (Some(ThinkingConfig::Disabled), 0)
    }
}

#[cfg(test)]
#[path = "choice_tests.rs"]
mod tests;
