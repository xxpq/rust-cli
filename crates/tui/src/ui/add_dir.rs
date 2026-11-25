//! `/add-dir <path>` 的确认框（docs/specs/rust-cli/permission-updates.md §6）：内核检查过、能加的目录才弹框。
//! 三项：只给这个会话、记住（存进本机项目规则集）、不加；esc 同不加。框用通用的选择列表（`ListView`），画在输入框的板上。
//!
//! 文字：英文见 permission-updates.md §6.1（标题、三项与暗色的说明），中文是本工程的。
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::user_command::LocalCommandOutput;
use rust_cli_kernel_api::working_dirs::{self, Remembered};

use super::app::{Action, App};
use super::i18n::Lang;
use super::list::{Choice, Item, ListView, Purpose};
use super::manage::Manage;

/// 确认框里选了哪一项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryAnswer {
    Session,
    Remember,
    Decline,
}

fn title(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Add directory to workspace",
        Lang::Zh => "把目录加入工作区",
    }
}

fn description(lang: Lang) -> &'static str {
    match lang {
        Lang::En => {
            "ZCode will be able to read files in this directory and make edits when auto-accept edits is on."
        }
        Lang::Zh => "ZCode 可以读这个目录里的文件；自动接受编辑开着时也可以改它们。",
    }
}

fn label(lang: Lang, answer: DirectoryAnswer) -> &'static str {
    match (lang, answer) {
        (Lang::En, DirectoryAnswer::Session) => "Yes, for this session",
        (Lang::En, DirectoryAnswer::Remember) => "Yes, and remember this directory",
        (Lang::En, DirectoryAnswer::Decline) => "No",
        (Lang::Zh, DirectoryAnswer::Session) => "是，只给这个会话",
        (Lang::Zh, DirectoryAnswer::Remember) => "是，并记住这个目录",
        (Lang::Zh, DirectoryAnswer::Decline) => "否",
    }
}

/// 加进去之后输入框上方的一句（没有终端转义的写法；记进对话的是 kernel-api 的 `added_record`）。
pub(crate) fn added(lang: Lang, directory: &str, remembered: &Remembered) -> String {
    match (lang, remembered) {
        (Lang::En, Remembered::No) => {
            format!("Added {directory} as a working directory for this session")
        }
        (Lang::En, Remembered::Saved) => {
            format!("Added {directory} as a working directory and saved to local settings")
        }
        (Lang::En, Remembered::Failed { reason }) => format!(
            "Added {directory} as a working directory. Failed to save to local settings: {reason}"
        ),
        (Lang::Zh, Remembered::No) => format!("已把 {directory} 加为这个会话的工作目录"),
        (Lang::Zh, Remembered::Saved) => format!("已把 {directory} 加为工作目录，并存进本机设置"),
        (Lang::Zh, Remembered::Failed { reason }) => {
            format!("已把 {directory} 加为工作目录。没能存进本机设置：{reason}")
        }
    }
}

fn declined(lang: Lang, directory: &str) -> String {
    match lang {
        Lang::En => format!("Did not add {directory} as a working directory."),
        Lang::Zh => format!("没有把 {directory} 加为工作目录。"),
    }
}

impl App {
    /// 内核检查过能加：弹确认框（`path` 是用户给的参数，记录用）。
    pub(crate) fn ask_directory(&mut self, path: String, directory: EnvPath) {
        let items = [
            DirectoryAnswer::Session,
            DirectoryAnswer::Remember,
            DirectoryAnswer::Decline,
        ]
        .into_iter()
        .map(|answer| Item {
            label: label(self.lang, answer).to_owned(),
            value: Choice::Directory {
                path: path.clone(),
                directory: directory.clone(),
                answer,
            },
            ..Item::default()
        })
        .collect();
        let mut list = ListView::new(
            Purpose::AddDirectory,
            title(self.lang).to_owned(),
            items,
            String::new(),
        );
        list.subtitle = Some(format!(
            "{}\n{}",
            directory.as_str(),
            description(self.lang)
        ));
        self.show_list(list);
    }

    /// 选了一项（esc 同「不加」）：加的两项交给 runner（内核再检查一次再加）；不加时说一句、记进对话。
    pub(super) fn answer_directory(
        &mut self,
        path: String,
        directory: &EnvPath,
        answer: DirectoryAnswer,
    ) -> Option<Action> {
        let remember = match answer {
            DirectoryAnswer::Session => false,
            DirectoryAnswer::Remember => true,
            DirectoryAnswer::Decline => {
                self.info(declined(self.lang, directory.as_str()));
                self.records
                    .push(rust_cli_kernel_api::user_command::LocalCommandRecord {
                        name: working_dirs::COMMAND.to_owned(),
                        args: path,
                        output: LocalCommandOutput::Stdout(working_dirs::declined_record(
                            directory.as_str(),
                        )),
                        caveat: true,
                        meta: Vec::new(),
                    });
                return None;
            }
        };
        Some(Action::Manage(Manage::ConfirmDirectory { path, remember }))
    }
}

#[cfg(test)]
#[path = "add_dir_tests.rs"]
mod tests;
