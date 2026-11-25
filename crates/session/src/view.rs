//! 会话的视图（`Request::View`，从 actor.rs 分出，800 行上限）：会话 actor 按它此刻的状态拼一份 [`SessionView`]，前端由它建投影
//! （session-lifecycle.md §10.13、§10.35）。

use rust_cli_kernel_api::session_api::SessionView;

use crate::actor::Actor;

impl Actor {
    pub(crate) async fn view(&self) -> SessionView {
        SessionView {
            plan: self.execution.plan,
            pending: self
                .pending
                .iter()
                .map(|pending| (pending.id.clone(), pending.request.clone()))
                .collect(),
            queue: self.inbox.queued(),
            last_seq: self.log.last_seq(),
            revision: self.log.revision(),
            background: [self.background().await, self.background_agents().await].concat(),
            works: self.works.running(),
            peer_held: self.peers.views(),
            // 冷加载后不用等下一条回复（F27）：与 `context_changed` 同样两个函数。
            context_usage: self.context_occupancy(),
            context_notice: self.context_notice().unwrap_or_default(),
            output_speed: self.output_speed,
            turns: self.turns.clone(),
            inputs: self.inputs.clone(),
            sources: self.sources.clone(),
            output_heads: self.output_heads.clone(),
            cancelled: self
                .running
                .as_ref()
                .map(|running| running.cancelled.clone())
                .unwrap_or_default(),
            tool_times: self.tool_times.clone(),
            plans: self.plans.clone(),
            reverted: crate::rewind::reverted_prompts(&self.reverted),
            queue_hold: self.queue_hold,
            follow_up: self.follow_up,
            mode_grant: self.mode_grant.clone(),
            plan_switch: self.plan_switch.clone(),
            additional_directories: self.additional_directories.clone(),
            session_rules: self.session_rules.clone(),
            hooks: self.hook_rows.recorded().to_vec(),
            subagents: self.subagent_rows.runs().to_vec(),
            preferences: self.preferences,
            // 任务清单（K-S9，agents.md §7.2）：与冷快照同一个函数读清单文件，活会话与冷读同值；之后的变化是 `TasksChanged`
            tasks: self.deps.agents.task_list(&self.meta.id).await,
            running: self
                .running_start()
                .map(rust_cli_kernel_api::session_api::Running::Turn),
            ..SessionView::new(
                self.meta.clone(),
                [self.earlier.as_slice(), self.messages.as_slice()].concat(),
                self.selection.clone(),
                self.execution.mode,
            )
        }
    }
}
