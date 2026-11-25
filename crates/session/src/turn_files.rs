//! 一个回合改过的文件（docs/specs/rust-cli/session-lifecycle.md §10.42、§10.45）：这个回合的 checkpoint（[`of_turn`]）、每个（机器，路径）
//! 最早的写入前 → 最后的写入后（[`written`]）、同一次 diff 给出增删行数与 hunk（[`diff`]）。三处用这一套：回合任务在主循环之后调 [`read`]
//! （`launch.rs`：artifact 是文件 IO，不在 actor 里读）；`/rewind` 的选择器给每条可选的消息调 [`read`]（`rewind::options`）；
//! 查询（`turn_file_diffs`）调 [`TurnFiles::diffs`]。

use std::collections::{BTreeSet, HashMap, HashSet};

use rust_cli_base::line_diff::{self, Hunk};
use rust_cli_kernel_api::ids::{MachineName, MessageId, TurnId};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::session_api::{FileChange, TurnFileChange, TurnFileDiff};
use rust_cli_kernel_api::store::{ArtifactStore, LoadedSession, TurnRecord, WorkspaceCheckpoint};

use crate::rewind_plan::Artifact;

/// hunk 的上下文行数（jsdiff `structuredPatch` 的缺省，同 Node 的 `createFinalPatch`）。
const PATCH_CONTEXT: usize = 4;
/// hunk 里新增与删除的行的前缀（jsdiff 的写法；Node 的 `countPatchLines` 按它数）。
const ADDED: char = '+';
const REMOVED: char = '-';

/// 这个回合的 checkpoint（§10.45）：清单里发出这次写入的 assistant 消息属于这个回合的，按清单的顺序（写入顺序）。
/// 属于：显示顺序里从这个回合的记录的第一条消息起、到下一个回合的记录的第一条消息之前（同投影的分组）。
///
/// 修改（FILE-DIFF）：回合末以前按 `prompt` 是开回合的消息取；查询时不知道开回合的是哪条（回合记录只有第一条消息，斜杠命令的展开可能
/// 不留那条），所以两处都按 assistant 消息取。本工程写的回合里两种取法是同一组：写入都由这个回合的 assistant 消息发出。
pub(crate) fn of_turn<'a>(
    messages: impl IntoIterator<Item = &'a Message>,
    turns: &[TurnRecord],
    checkpoints: &[WorkspaceCheckpoint],
    turn: &TurnId,
) -> Vec<WorkspaceCheckpoint> {
    let firsts: HashMap<&MessageId, &TurnId> = turns
        .iter()
        .map(|record| (&record.first, &record.turn))
        .collect();
    let mut current: Option<&TurnId> = None;
    let mut ids: HashSet<&MessageId> = HashSet::new();
    for message in messages {
        if let Some(opened) = firsts.get(message.id()) {
            current = Some(opened);
        }
        if current == Some(turn) {
            ids.insert(message.id());
        }
    }
    checkpoints
        .iter()
        .filter(|checkpoint| ids.contains(&checkpoint.assistant))
        .cloned()
        .collect()
}

/// 一个（机器，路径）在这组写入里的累计：最早的写入前、最后的写入后、写了几次、哪些工具写的。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Written {
    pub machine: MachineName,
    pub path: String,
    pub before: String,
    pub after: String,
    pub writes: u64,
    pub tools: BTreeSet<String>,
}

async fn artifact(
    store: &dyn ArtifactStore,
    checkpoint: &WorkspaceCheckpoint,
) -> Result<Artifact, String> {
    let bytes = store
        .get(&checkpoint.snapshot)
        .await
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

/// 按写入顺序累计（纯计算）：同一（机器，路径）的第一次写入给出「写入前」，最后一次给出「写入后」。按路径排，同一路径主 env 在前、
/// 额外机器按名字（远端执行 F98，remote-exec.md §25.4.5：两台机器上的同名文件不合在一起；只有本机时就是按路径排）。
pub(crate) fn accumulate(artifacts: &[(MachineName, Artifact)]) -> Vec<Written> {
    let mut out: Vec<Written> = Vec::new();
    for (machine, artifact) in artifacts {
        for file in &artifact.files {
            let after = file.after_content.clone().unwrap_or_default();
            match out
                .iter_mut()
                .find(|entry| entry.path == file.path && entry.machine == *machine)
            {
                Some(entry) => {
                    entry.after = after;
                    entry.writes += 1;
                    entry.tools.insert(artifact.tool_name.clone());
                }
                None => out.push(Written {
                    machine: machine.clone(),
                    path: file.path.clone(),
                    before: file.before_content.clone().unwrap_or_default(),
                    after,
                    writes: 1,
                    tools: BTreeSet::from([artifact.tool_name.clone()]),
                }),
            }
        }
    }
    out.sort_by(|a, b| (&a.path, a.machine.remote()).cmp(&(&b.path, b.machine.remote())));
    out
}

/// 读这组 checkpoint 的 artifact（读不出的跳过：那个文件不进汇总，同 `/rewind` 的选择器）再累计。
async fn written(store: &dyn ArtifactStore, checkpoints: &[WorkspaceCheckpoint]) -> Vec<Written> {
    let mut artifacts = Vec::new();
    for checkpoint in checkpoints {
        if let Ok(read) = artifact(store, checkpoint).await {
            artifacts.push((checkpoint.machine.clone(), read));
        }
    }
    accumulate(&artifacts)
}

/// 一个文件的 diff（§10.45）：增删行数是 hunk 里 `+` / `-` 开头的行（同 Node 的 `countPatchLines`）。编辑距离超过上限时没有 hunk，
/// 增删行数是去掉公共的首尾行之后两边剩下的行数（rust 原有的做法；Node 的 diff 超时时是 0）。
///
/// 修 bug（FILE-DIFF）：以前按 `\n` 切行、去掉最后一个换行再求最小编辑距离，只差末尾换行的改动（`a` → `a\n`）算成 +0/−0；jsdiff 与
/// Node 是 +1/−1（最后一行带不带换行是两个不同的行）。现在数同一次 diff 的 hunk，与查询给的 hunk 一致。
pub(crate) fn diff(before: &str, after: &str) -> (u64, u64, Vec<Hunk>) {
    let Some(hunks) = line_diff::structured_patch(before, after, PATCH_CONTEXT) else {
        let (old, new) = (
            before.split_inclusive('\n').collect::<Vec<_>>(),
            after.split_inclusive('\n').collect::<Vec<_>>(),
        );
        let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let count = |lines: &[&str]| (lines.len() - prefix - suffix) as u64;
        return (count(&new), count(&old), Vec::new());
    };
    let count = |sign: char| {
        hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.starts_with(sign))
            .count() as u64
    };
    (count(ADDED), count(REMOVED), hunks)
}

fn change(written: &Written) -> TurnFileChange {
    let (additions, deletions, _) = diff(&written.before, &written.after);
    TurnFileChange {
        change: FileChange {
            path: written.path.clone(),
            additions,
            deletions,
            machine: written.machine.clone(),
        },
        writes: written.writes,
        tools: written.tools.iter().cloned().collect(),
    }
}

/// 这组 checkpoint（一个回合的，按写入顺序）合起来的改动。
pub(crate) async fn read(
    store: &dyn ArtifactStore,
    checkpoints: &[WorkspaceCheckpoint],
) -> Vec<TurnFileChange> {
    written(store, checkpoints)
        .await
        .iter()
        .map(change)
        .collect()
}

/// 查询用的快照（§10.45）：这个回合的记录里的文件改动与它的 checkpoint。打开着的会话由 actor 在一次处理里给出，没打开的由存储给的状态算
/// （就是 actor 的起始状态）；读 artifact 在调用方的任务里（[`Self::diffs`]）。
pub(crate) struct TurnFiles {
    files: Vec<TurnFileChange>,
    checkpoints: Vec<WorkspaceCheckpoint>,
}

impl TurnFiles {
    pub(crate) fn of<'a>(
        messages: impl IntoIterator<Item = &'a Message>,
        turns: &[TurnRecord],
        checkpoints: &[WorkspaceCheckpoint],
        turn: &TurnId,
    ) -> Self {
        let files: Vec<TurnFileChange> = turns
            .iter()
            .find(|record| record.turn == *turn)
            .and_then(|record| record.ended.as_ref())
            .map(|ended| ended.files.clone())
            .unwrap_or_default();
        // 记录里没有文件改动（回合还在跑、旧记录）时不读 artifact：数字不从 checkpoint 补（§10.42）
        let checkpoints = if files.is_empty() {
            Vec::new()
        } else {
            of_turn(messages, turns, checkpoints, turn)
        };
        Self { files, checkpoints }
    }

    /// 读 artifact，按（机器，路径）把 hunk 接到记录的文件上。数字是记录里的（与回合头同一份）；记录里有、这里算不出的文件（artifact 读不出、
    /// 超过编辑距离上限）hunk 为空。撤销过的回合的 checkpoint 由调用方一起给（§10.54）。
    pub(crate) async fn diffs(self, store: &dyn ArtifactStore) -> Vec<TurnFileDiff> {
        let written = written(store, &self.checkpoints).await;
        self.files
            .into_iter()
            .map(|file| {
                let hunks = written
                    .iter()
                    .find(|entry| {
                        entry.path == file.change.path && entry.machine == file.change.machine
                    })
                    .map(|entry| diff(&entry.before, &entry.after).2)
                    .unwrap_or_default();
                TurnFileDiff { file, hunks }
            })
            .collect()
    }
}

/// 没打开的会话的回合文件 diff（runtime 冷加载之后，§10.45）：存储给的消息、回合记录与 checkpoint 就是会话打开之后 actor 的起始状态，
/// 同一个取法与算法。
pub async fn turn_file_diffs(
    artifacts: &dyn ArtifactStore,
    loaded: &LoadedSession,
    turn: &TurnId,
) -> Vec<TurnFileDiff> {
    TurnFiles::of(
        loaded.earlier.iter().chain(&loaded.messages),
        &loaded.turns,
        &[loaded.checkpoints.as_slice(), loaded.reverted.as_slice()].concat(),
        turn,
    )
    .diffs(artifacts)
    .await
}

#[cfg(test)]
#[path = "turn_files_tests.rs"]
mod tests;
