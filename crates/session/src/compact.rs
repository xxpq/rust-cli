//! 手动压缩（`Command::Compact`，docs/specs/rust-cli/compact.md §4.2）与压缩结果的提交。
//!
//! `/compact` 作为一个回合运行：与主循环共用 [`Running`]，所以中断、排队与收尾都走同一条路径；
//! 压缩任务经 [`ActorSink`] 把边界、摘要与附件交给 actor，在一个存储事务里提交。

use crate::actor::Actor;
use rust_cli_kernel_api::compaction::{
    ContextNotice, ContextOccupancy, LoopCompaction, LoopCompactionEnd,
};
use rust_cli_kernel_api::ids::{CommandId, TurnId};
use rust_cli_kernel_api::input_extras::TurnConfig;
use rust_cli_kernel_api::message::{CompactTrigger, Message, SystemMessage};
use rust_cli_kernel_api::orchestration::{CompactRequest, QueryStop, RunSink};
use rust_cli_kernel_api::session_api::KernelEvent;
use rust_cli_kernel_api::user_command::{LocalCommandOutput, LocalCommandRecord, dim};

/// 压缩任务提交请求的缓冲。
const SINK_REQUESTS: usize = 16;

/// `/compact` 成功时的输出（对照实现的原文，带终端的暗色）。
const COMPACTED: &str = "Compacted (ctrl+o to see full summary)";

/// `/compact` 的命令记录：前面加固定说明（对照实现的 local 类型）。
fn compact_record(args: &str, output: LocalCommandOutput) -> LocalCommandRecord {
    LocalCommandRecord {
        name: "compact".to_owned(),
        args: args.to_owned(),
        output,
        caveat: true,
        meta: Vec::new(),
    }
}

impl Actor {
    pub(crate) fn start_compaction(&mut self, command: CommandId, instructions: Option<String>) {
        let turn = self.deps.ids.turn();
        self.turn_started(
            turn.clone(),
            Some(command),
            rust_cli_kernel_api::session_api::TurnOrigin::Compaction,
        );
        self.log.append(KernelEvent::CompactStarted {
            trigger: CompactTrigger::Manual,
        });
        // `/compact` 的命令记录（修复线 L-A，tui-manage.md §11.2）：照对照实现排在压缩结果之后、同一批（保留的消息
        // 之后接固定说明、命令输入与输出）；失败时记失败原因。
        let args = instructions.clone().unwrap_or_default();
        let ids = self.deps.ids.clone();
        let request = CompactRequest {
            session: self.meta.id.clone(),
            workspace: self.meta.workspace.clone(),
            trigger: CompactTrigger::Manual,
            selection: self.selection.clone(),
            instructions,
            messages: self.messages.clone(),
            mode: Some(self.mode()),
            agent: None,
            // `/compact` 只在顶层会话里
            root: None,
            initial_token_gap: None,
            additional_directories: self.additional_directories.clone(),
            preferences: self.preferences,
            prompt: self.prompt.current(),
        };
        let opening = self.open_compaction(turn, SINK_REQUESTS);
        let sink = opening.sink.clone();
        let compactor = self.deps.compactor.clone();
        let signal = opening.abort.clone();
        // SessionStart（`compact`）在压缩器里跑，消息已在 `restored` 末尾（compact.md §7），与边界、
        // 摘要一个事务提交；这里不再单独跑（以前只有 `/compact` 跑，自动 / 反应式压缩漏掉了）。
        let task = self.deps.spawner.spawn("compact-run", async move {
            // 摘要请求的计量经这个压缩回合的输出端提交（usage.md §2.1.3）：会话 actor 把它们记在压缩回合上。
            match compactor
                .compact_metered(request, signal.token().clone(), sink.as_ref())
                .await
            {
                Ok(result) => {
                    let mut messages = vec![
                        Message::System(SystemMessage::CompactBoundary(result.boundary)),
                        Message::User(result.summary),
                    ];
                    messages.extend(result.restored);
                    messages.extend(crate::local_record::record_messages(
                        ids.as_ref(),
                        &compact_record(&args, LocalCommandOutput::Stdout(dim(COMPACTED))),
                    ));
                    sink.commit_messages(messages).await?;
                    Ok(QueryStop::Completed)
                }
                Err(_) if signal.is_aborted() => Ok(QueryStop::Aborted),
                // 失败原因是给用户的原文（例如 `Not enough messages to compact.`）。
                Err(error) => {
                    let message = error.to_string();
                    let record = compact_record(&args, LocalCommandOutput::Stderr(message.clone()));
                    sink.commit_messages(crate::local_record::record_messages(
                        ids.as_ref(),
                        &record,
                    ))
                    .await?;
                    Ok(QueryStop::ApiError { message })
                }
            }
        });
        self.launch(opening, task, None, Vec::new());
    }

    /// v5 的压缩（`Command::CompactOrQueue`，session-lifecycle.md §10.23）：准入给的是排队就排到队尾（轮到它时 `start_next` 开压缩的
    /// 回合），否则现在开。它不是输入，排进去不发输入的结局。`selection` 是这一次提交的模型（§10.30，照 v4）：开压缩的回合时先写成
    /// 会话的模型（与输入开回合同一条路），只有模型，模式与 plan 不动。
    pub(crate) async fn compact_or_queue(
        &mut self,
        command: CommandId,
        selection: Option<Box<rust_cli_kernel_api::model::ModelSelection>>,
        admission: &rust_cli_kernel_api::admission::Admission,
    ) -> rust_cli_kernel_api::session_api::Ack {
        let config = TurnConfig {
            selection,
            ..TurnConfig::default()
        };
        if matches!(
            admission,
            rust_cli_kernel_api::admission::Admission::Enqueue(_)
        ) {
            self.inbox.push_compaction(command, config);
            self.emit_queue();
            return crate::actor::accepted();
        }
        match self
            .start_turn(crate::turn::TurnInput::Compact { command, config })
            .await
        {
            Ok(()) => crate::actor::accepted(),
            Err(reason) => {
                crate::actor::rejected(rust_cli_kernel_api::admission::Refusal::Failed, reason)
            }
        }
    }

    /// 按当前历史与模型重算“上下文快满”的提示与 footer 的 ctx 并发出（压缩模块给出级别、数字与 `(token, 有效窗口)`，
    /// compact.md §2.1、tui-display.md §2.6）；压缩模块不知道的那一项不发。调用处：提交带用量的 assistant、提交压缩
    /// 结果、换模型（F27：窗口随模型变）。快照（`view` 方法）用同样两个函数。
    pub(crate) fn context_changed(&self) {
        if let Some(notice) = self.context_notice() {
            self.log.append(KernelEvent::ContextNotice { notice });
        }
        if let Some(ContextOccupancy {
            used_tokens,
            max_tokens,
            compact_at,
            window,
        }) = self.context_occupancy()
        {
            self.log.append(KernelEvent::ContextUsage {
                used_tokens,
                max_tokens,
                compact_at,
                window,
            });
        }
    }

    pub(crate) fn context_notice(&self) -> Option<ContextNotice> {
        self.deps
            .compactor
            .context_notice(&self.messages, &self.selection)
    }

    pub(crate) fn context_occupancy(&self) -> Option<ContextOccupancy> {
        self.deps
            .compactor
            .context_usage(&self.messages, &self.selection)
    }
}

/// 主循环开始压缩时的事件（compact.md §4.1），与这一轮回合记录里的一项（session-lifecycle.md §10.52）。
pub(crate) fn compact_started(actor: &mut Actor, trigger: CompactTrigger) {
    actor.log.append(KernelEvent::CompactStarted { trigger });
    let Some(turn) = actor.running.as_ref().map(|running| running.turn.clone()) else {
        return;
    };
    let started_at = actor.deps.clock.now_ms();
    let after = actor.messages.last().map(|message| message.id().clone());
    actor.change_loop_compactions(&turn, |compactions| {
        compactions.push(LoopCompaction {
            trigger,
            started_at,
            after: after?,
            ended: None,
        });
        Some(compactions.len() - 1)
    });
}

impl Actor {
    /// 主循环里在跑的那次压缩有了结局（session-lifecycle.md §10.52）：写进 `turn` 的回合记录，发 `CompactionRecorded`。没有在跑的那一项
    /// （`/compact` 的回合、已经写过结局的）不动。
    pub(crate) fn loop_compaction_ended(&mut self, turn: &TurnId, end: LoopCompactionEnd) {
        self.change_loop_compactions(turn, |compactions| {
            let index = compactions.len().checked_sub(1)?;
            let last = &mut compactions[index];
            if last.ended.is_some() {
                return None;
            }
            last.ended = Some(end);
            Some(index)
        });
    }

    /// 改 `turn` 的回合记录里主循环的压缩（只在内存，回合结束时与结局同一次写，§10.52）：`change` 返回改了的那一项的位置，改了就发
    /// `CompactionRecorded`。先改记录、再发事件，所以视图与事件的交界不漏不重。回合记录在这一轮第一条消息落库时就有了，主循环在输入落库
    /// 之后才跑，所以总找得到（同 `fell_back`）。
    fn change_loop_compactions(
        &mut self,
        turn: &TurnId,
        change: impl FnOnce(&mut Vec<LoopCompaction>) -> Option<usize>,
    ) {
        let Some(record) = self
            .turns
            .iter_mut()
            .rev()
            .find(|record| record.turn == *turn)
        else {
            return;
        };
        if let Some(index) = change(&mut record.compactions) {
            self.log.append(KernelEvent::CompactionRecorded {
                turn: turn.clone(),
                index,
                compaction: record.compactions[index].clone(),
            });
        }
    }
}

#[cfg(test)]
#[path = "compact_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "compacted_view_tests.rs"]
mod compacted_view_tests;
