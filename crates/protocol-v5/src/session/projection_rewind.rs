//! 撤销过文件改动的回合（protocol-v5.md §53.5，session-lifecycle.md §10.54）：回合头的文件改动标成 `reverted`（同 v4 的 `fileChanges.state`）。
//! 活会话的 `FilesRestored` 与由视图建时视图的 `reverted` 同一个函数，所以活冷同值。

use rust_cli_kernel_api::ids::MessageId;

use super::Projection;
use super::edit::Edit;
use super::rows::{self, ROWS, keys};

impl Projection {
    /// 这些开回合输入所在的回合：回合头带文件改动的，汇总的状态写成 `reverted`。输入的行不在（截掉的分支、冷读的压缩点之前没有它）、回合头没有
    /// 文件改动的，不动。
    pub(super) fn files_restored(&self, edit: &mut Edit<'_>, prompts: &[MessageId]) {
        for prompt in prompts {
            let Some(input) = edit.get(ROWS, prompt.as_str()) else {
                continue;
            };
            let header = rows::text(input, keys::TURN_ID).to_owned();
            if let Some(mut fields) = edit.get(ROWS, &header).cloned()
                && rows::set_reverted(&mut fields)
            {
                edit.put(ROWS, &header, fields);
            }
        }
    }
}
