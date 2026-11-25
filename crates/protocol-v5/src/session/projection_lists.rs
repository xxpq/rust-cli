//! 投影里行之外的部分（从 projection.rs 分出，800 行上限）：交互、队列、输入的结局与 state 区的键（02 §1.1、§1.5、§7）。

use rust_cli_kernel_api::session_api::{Joined, Running, TurnTime};

use super::*;

/// 队列项的种类（`QUEUE_ITEM_KINDS`）。
const QUEUE_KIND_INPUT: &str = "input";
const QUEUE_KIND_COMPACT: &str = "compact";
/// 队列项 `intent.delivery` 的两种写法（App 的 `conversationInputDeliverySchema`）。
const DELIVERY_QUEUE: &str = "queue";
const DELIVERY_GUIDE: &str = "guide";
/// 由视图建的 queued 项的 `at`：视图里没有排进队列的时刻（内核的队列项不带时刻，§56.2）。`inputs` 的读者只看 `fate`。
const QUEUED_AT_UNKNOWN: u64 = 0;
/// 队列项的编辑锁（02 §11.10 A1）。
const EDIT_LOCK_KEY: &str = "editLock";

impl Projection {
    /// 主线程模型请求的通知（stall-visibility.md §3）：重试时 `control.apiRetry` 带这一次的重试，停顿与恢复清掉它（从 projection.rs 的事件分派里
    /// 分出，800 行上限）。
    pub(super) fn model_notice(&mut self, edit: &mut Edit<'_>, notice: &ModelNotice, at: u64) {
        self.retry = match notice {
            ModelNotice::Retrying {
                attempt,
                max_retries,
                delay_ms,
                error,
            } => Some(state::api_retry(
                *attempt,
                *max_retries,
                error,
                at,
                *delay_ms,
            )),
            ModelNotice::Stalled { .. } | ModelNotice::Resumed => None,
        };
        self.control(edit);
    }

    /// 投影里看得到的工作都没有（热会话回收的预选，protocol-v5.md §69.0 第 3 条）：没有在跑的回合、待回答的交互与校验、在跑的后台工作、
    /// 排队的项，也不是草稿。只用来挑候选：空闲与否以会话 actor 的判断为准。
    pub(crate) fn quiet(&self) -> bool {
        self.running.is_none()
            && self.interactions.is_empty()
            && self.verifications.is_empty()
            && !self.has_works()
            && self.queue.is_empty()
            && self.phase() != Phase::Draft
    }

    /// 通道关了会话运行时（热会话回收，protocol-v5.md §69.0 第 6 条）：投影变冷，同一批里 `control.runtime` 是 cold、`mcpAdmission` 回到冷读的
    /// null（18 §12.5：没有会话 actor 就没有准入）。
    pub(crate) fn went_cold(&mut self) -> Batch {
        self.live = false;
        self.mcp = Value::Null;
        let mut edit = Edit::new(None);
        self.control(&mut edit);
        edit.state(state::keys::MCP_ADMISSION, Value::Null);
        edit.into_batch()
    }

    /// 这个会话在会话列表里的活性（`live`，04 §3）：回合在跑、有待回答的交互或有后台工作时有；删除了的没有。`publisher` 是这个会话的 topic
    /// （交互实体的版本）。
    pub(crate) fn hot<'a>(&'a self, publisher: Option<&Publisher>) -> Option<live::Hot<'a>> {
        let running = self.running.is_some();
        // 有后台工作在跑的会话也是 hot（04 §4.1，protocol-v5.md §20.2）：回合结束时它留在 live 里，只改 phase
        // 有待回答的校验时也是 hot（§35.2）：标题等回合外的模型请求也可能要校验
        let idle = self.interactions.is_empty() && self.verifications.is_empty();
        if self.deleted || (!running && idle && !self.has_works()) {
            return None;
        }
        let version =
            |id: &str| publisher.and_then(|publisher| publisher.version(INTERACTIONS, id));
        let kernel = self
            .interactions
            .iter()
            .map(|(id, (_, request))| live::Asked {
                id,
                v: version(id),
                kind: interactions::kind(request),
                tool: Some(interactions::tool_name(request)),
            });
        let front = self.verifications.keys().map(|id| live::Asked {
            id,
            v: version(id),
            kind: crate::session::verify::REQUEST_VERIFICATION,
            tool: None,
        });
        let pending = kernel.chain(front).collect();
        Some(live::Hot {
            running,
            outcome: self.outcome,
            pending,
            background: self.has_works(),
        })
    }

    pub(super) fn asked(
        &mut self,
        edit: &mut Edit<'_>,
        interaction: &InteractionId,
        request: &InteractionRequest,
    ) {
        let id = interactions::entity_id(request);
        edit.put(INTERACTIONS, &id, interactions::fields(request));
        if matches!(request, InteractionRequest::Permission { .. })
            && let Some(mut fields) = edit
                .get(ROWS, &id)
                .filter(|fields| !tool_status::ended(rows::text(fields, keys::STATUS)))
                .cloned()
        {
            rows::set_status(&mut fields, tool_status::PENDING_APPROVAL);
            fields.insert(keys::APPROVAL.to_owned(), json!(id));
            edit.put(ROWS, &id, fields);
        }
        self.interactions
            .insert(id, (interaction.clone(), request.clone()));
    }

    pub(super) fn resolved(&mut self, edit: &mut Edit<'_>, interaction: &InteractionId) {
        let Some(id) = self
            .interactions
            .iter()
            .find(|(_, (kernel, _))| kernel == interaction)
            .map(|(id, _)| id.clone())
        else {
            return;
        };
        self.interactions.remove(&id);
        edit.remove(INTERACTIONS, &id);
        if let Some(mut fields) = edit
            .get(ROWS, &id)
            .filter(|fields| rows::text(fields, keys::STATUS) == tool_status::PENDING_APPROVAL)
            .cloned()
        {
            rows::set_status(&mut fields, tool_status::RUNNING);
            fields.remove(keys::APPROVAL);
            edit.put(ROWS, &id, fields);
        }
    }

    /// 整份队列：按 id 比，新的与变了的 put，没了的 remove；顺序写进 state 的 `queueOrder`（一批，§15.3.2、§23.1）。
    ///
    /// 修 bug（13 Q24）：以前顺序是每一项的 `order.queuePosition`，前一项出队时后面每一项都 put、`v` 都变，用户正在改的那一项被拒。
    /// 顺序放到队列这一级之后，别的项走了，这一项的字段不变（Publisher 去掉没变的 put），`v` 也不变。
    pub(super) fn queued(&mut self, edit: &mut Edit<'_>, items: &[QueuedInput]) {
        let now: BTreeSet<&CommandId> = items.iter().map(|item| &item.command).collect();
        for gone in self
            .queue
            .iter()
            .filter(|item| !now.contains(&item.command))
        {
            edit.remove(QUEUE, gone.command.as_str());
            self.admitted.remove(&gone.command);
        }
        for item in items {
            let admission = *self
                .admitted
                .entry(item.command.clone())
                .or_insert_with(|| {
                    self.next_admission += 1;
                    self.next_admission
                });
            // 排队的压缩（§17.3）是 `kind: compact`，正文为空
            let kind = if item.compact {
                QUEUE_KIND_COMPACT
            } else {
                QUEUE_KIND_INPUT
            };
            let fields: Fields = [
                ("kind", json!(kind)),
                ("text", json!(item.text)),
                ("sourceCommandId", json!(item.command.as_str())),
                (
                    inputs::ATTACHMENTS_KEY,
                    inputs::attachments(&item.extras.files, &item.extras.uploads),
                ),
                ("dispatch", json!({"state": "waiting"})),
                ("order", json!({"admissionSeq": admission})),
            ]
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect();
            let mut fields = fields;
            if !item.extras.quotes.is_empty() {
                fields.insert(
                    inputs::QUOTES_KEY.to_owned(),
                    inputs::quotes(&item.extras.quotes),
                );
            }
            inputs::config_fields(&mut fields, &item.extras.config);
            // 这一项怎么投递（§22.1）：插话项等这一轮的工具边界并入，界面把它画成引导而不是下一轮的排队。修 bug：以前不给，
            // 界面把插话项都当成排队项画在队列里。内核只知道准入给的投递，所以要求的与准入的同值
            let delivery = match item.delivery {
                Delivery::Queue => DELIVERY_QUEUE,
                Delivery::Steer => DELIVERY_GUIDE,
            };
            fields.insert(
                "intent".to_owned(),
                json!({"delivery": {"requested": delivery, "admitted": delivery}}),
            );
            // Composer 编辑态的锁（§64.1）：有锁才写，界面按 editorId 判本端 / 他端
            if let Some(lock) = &item.edit_lock {
                fields.insert(EDIT_LOCK_KEY.to_owned(), json!({"editorId": lock.editor}));
            }
            edit.put(QUEUE, item.command.as_str(), fields);
        }
        let order: Vec<&str> = items.iter().map(|item| item.command.as_str()).collect();
        edit.state(state::keys::QUEUE_ORDER, json!(order));
        self.queue = items.to_vec();
    }

    /// 由视图建投影时（物化、激活、改写历史，§15.2）视图里还没有结局的输入：排着的是 queued，开了在跑回合的是 started（02 §1.5：非终态一直在集合里）。
    ///
    /// 修 bug（EXP-DEFECTS-1，§56.2）：以前 `inputs` 只由 `InputFate` 事件写，由视图建时从空开始；视图之前的结局 seq 不大于 `last_seq`、不再应用。
    /// 编辑、重试开的回合在 `HistoryRewritten` 之后、通道取新视图之前就开了，改写之后的快照里于是没有在跑的那条输入，排着的也没了，回合结束才有终态。
    /// 事实只取视图（内核的队列与在跑回合的开始），不补发事件。
    pub(super) fn viewed_inputs(&mut self, edit: &mut Edit<'_>, view: &SessionView) {
        for item in view.queue.iter().filter(|item| !item.compact) {
            self.fated(edit, &item.command, &InputFate::Queued, QUEUED_AT_UNKNOWN);
        }
        // 开回合的输入：进回合的时刻取回合开始的时刻（同一次处理发 `TurnStarted` 与它的结局，两次读时钟，至多差几毫秒）。
        // 压缩回合的命令不是输入，内核不为它发结局
        if let Some(start) = view.running.as_ref().and_then(Running::turn)
            && start.origin == TurnOrigin::Input
            && let Some(command) = &start.command
        {
            let fate = InputFate::Started {
                turn: start.turn.clone(),
                joined: Joined::Opened,
            };
            self.fated(edit, command, &fate, start.started_at);
        }
    }

    pub(super) fn fated(
        &mut self,
        edit: &mut Edit<'_>,
        input: &CommandId,
        fate: &InputFate,
        at: u64,
    ) {
        let (name, turn, reason) = match fate {
            InputFate::Queued => ("queued", None, None),
            InputFate::Started { turn, .. } => ("started", Some(turn), None),
            InputFate::Ended { turn, end } => (
                match end {
                    InputEnd::Completed => "completed",
                    InputEnd::Cancelled => "cancelled",
                    InputEnd::Failed => "failed",
                },
                Some(turn),
                None,
            ),
            InputFate::Dropped {
                why: DropReason::TakenBack,
            } => ("discarded", None, None),
            InputFate::Dropped {
                why: DropReason::Blocked,
            } => ("failed", None, Some(codes::ADMISSION_INPUT_REJECTED)),
        };
        let mut fields = Fields::new();
        fields.insert("fate".to_owned(), json!(name));
        if let Some(turn) = turn {
            fields.insert("turnId".to_owned(), json!(turn.as_str()));
        }
        if let Some(reason) = reason {
            fields.insert("reason".to_owned(), json!(reason));
        }
        fields.insert("at".to_owned(), json!(at));
        edit.put(INPUTS, input.as_str(), fields);
        self.fates.insert(input.as_str().to_owned(), fate.clone());
        if matches!(fate, InputFate::Ended { .. } | InputFate::Dropped { .. }) {
            self.terminal.push_back(input.as_str().to_owned());
            while self.terminal.len() > INPUT_FATE_RETAIN {
                if let Some(oldest) = self.terminal.pop_front() {
                    self.fates.remove(&oldest);
                    edit.remove(INPUTS, &oldest);
                }
            }
        }
    }

    pub(super) fn phase(&self) -> Phase {
        match (self.deleted, &self.running, self.outcome) {
            (true, _, _) => Phase::Deleted,
            (false, Some(_), _) => Phase::Running,
            (false, None, Some(Outcome::Error)) => Phase::Error,
            (false, None, Some(_)) => Phase::Completed,
            (false, None, None) if self.draft => Phase::Draft,
            (false, None, None) => Phase::Idle,
        }
    }

    pub(super) fn control(&self, edit: &mut Edit<'_>) {
        let facts = ControlFacts {
            phase: self.phase(),
            live: self.live,
            running: self.running.as_ref(),
            outcome: self.outcome,
            error: self.error.as_ref(),
            retry: self.retry.as_ref(),
        };
        edit.state(state::keys::CONTROL, state::control(&facts));
    }

    pub(super) fn meta(&self) -> Value {
        state::meta(
            &self.title,
            self.title_source,
            self.created_at,
            self.side_chat,
        )
    }

    pub(super) fn config(&self) -> Value {
        let hints = state::Hints {
            grant: self.grant.as_ref(),
            plan_switch: self.plan_switch.as_ref(),
        };
        state::config(
            &self.selection,
            self.mode,
            self.plan,
            self.follow_up,
            &hints,
        )
    }

    pub(super) fn admission(&self) -> Value {
        let held = self.hold.is_some() && !self.queue.is_empty();
        // 有压缩在跑或在排（§17.3）：同内核 AdmissionView.compaction；提升了还没开的压缩投影看不到，内核按它拒绝
        let compaction = self.running.as_ref().is_some_and(|running| running.compact)
            || self.queue.iter().any(|queued| queued.compact);
        // 队首被编辑锁冻结（§64.1）：同内核 AdmissionView.frozen（第一条排在回合之后的输入锁着）
        let frozen = self
            .queue
            .iter()
            .find(|queued| queued.delivery == Delivery::Queue)
            .is_some_and(|queued| queued.edit_lock.is_some());
        state::admission(&state::Gates {
            running: self.running.as_ref().map(|r| &r.turn),
            held,
            frozen,
            deleted: self.deleted,
            draft: self.phase() == Phase::Draft,
            compaction,
            follow_up: self.follow_up,
            side_chat: self.side_chat,
        })
    }

    pub(super) fn window(&self, edit: &mut Edit<'_>) {
        let first = self.order.first().map(|(_, id)| id.as_str());
        edit.state(
            state::keys::ROWS_WINDOW,
            state::rows_window(self.order.len(), first, self.earlier),
        );
    }

    pub(super) fn all_state(&self, edit: &mut Edit<'_>) {
        self.control(edit);
        edit.state(state::keys::META, self.meta());
        edit.state(state::keys::CONFIG, self.config());
        edit.state(state::keys::USAGE, state::usage(self.context.as_ref()));
        for (key, value) in state::defaults() {
            edit.state(key, value);
        }
        edit.state(state::keys::QUEUE_POLICY, state::queue_policy(self.hold));
        edit.state(state::keys::ADMISSION, self.admission());
        edit.state(state::keys::MCP_ADMISSION, self.mcp.clone());
        edit.state(state::keys::NOTICES, Value::Array(self.notices.clone()));
        self.window(edit);
    }

    /// 工具行读成「已取消」（停止时它在跑或在等回答，02 §11 C7）。
    pub(super) fn cancel_row(&self, edit: &mut Edit<'_>, id: &str) {
        if let Some(mut fields) = edit.get(ROWS, id).cloned() {
            rows::set_status(&mut fields, tool_status::CANCELLED);
            edit.put(ROWS, id, fields);
        }
    }

    pub(super) fn finished(
        &mut self,
        edit: &mut Edit<'_>,
        turn: &TurnId,
        outcome: &TurnOutcome,
        time: TurnTime,
        files: &[rust_cli_kernel_api::session_api::TurnFileChange],
    ) {
        // 这一轮的 UserPromptSubmit 拦下（§49.2）：横幅留过这一轮的成功
        let block = self
            .hook_block
            .take()
            .filter(|block| block.turn == turn.as_str());
        let (outcome, state) = match outcome {
            // 被拦下的输入的回合照 v4 读成成功。修改（SMALL-7，§49.8）：以前内核给 `Failed`，这里按 `HooksRan.blocked` 留下的事实改读；
            // 由视图建的投影没有那条事实，读成失败。现在内核直接给 `PromptBlocked`
            TurnOutcome::Success
            | TurnOutcome::HookStopped
            | TurnOutcome::StopHookPrevented
            | TurnOutcome::PromptBlocked { .. } => {
                self.error = block.map(|block| block.error);
                (Outcome::Success, turn_state::COMPLETED)
            }
            TurnOutcome::Interrupted => {
                self.error = None;
                (Outcome::Interrupted, turn_state::CANCELLED)
            }
            TurnOutcome::Failed { message, cause } => {
                // 这一轮用的卡（§46.1）：每个新错误都带，界面据此不按错误码猜要不要作废卡
                let attribution = self.attribution(edit, turn.as_str());
                self.error = Some(state::last_error(message, cause.as_ref(), attribution));
                (Outcome::Error, turn_state::ERROR)
            }
        };
        // 压缩的回合结束：开始的标记改成失败、或去掉，与回合头、`lastError` 同一批（§23.1、§47.1）
        if let Some(running) = self.running.clone() {
            self.compaction_ended(edit, &running, outcome);
        }
        self.outcome = Some(outcome);
        self.running = None;
        self.retry = None;
        if let Some(mut fields) = edit.get(ROWS, turn.as_str()).cloned() {
            fields.insert(keys::STATE.to_owned(), json!(state));
            fields.insert(keys::STABLE.to_owned(), json!(true));
            // 结束的时刻与用时、最后一段的收口（§29.1）、这个回合的文件改动（§42.1）：与冷读读回合记录同一组函数、同一个值
            self.turn_ended(&mut fields, time);
            rows::set_files(&mut fields, files);
            edit.put(ROWS, turn.as_str(), fields);
        }
        // 没等到 MessageDone 的流式行（请求失败、整段丢掉重发，§15.3.5）：去掉
        let unfinished: Vec<MessageId> = self
            .streams
            .iter()
            .filter(|(_, stream)| stream.turn == turn.as_str())
            .map(|(message, _)| message.clone())
            .collect();
        for message in unfinished {
            if let Some(stream) = self.streams.remove(&message) {
                for id in &stream.rows {
                    if !self.open_tools.contains_key(id) {
                        self.remove_row(edit, id);
                    }
                }
            }
        }
        // 没等到 MessageDone 的续写放回续写之前的正文（§19.9）
        self.settle_continuations(edit);
        // 还没结束的工具：cancelled（02 §11 C7），结束的时刻是回合的结束（§31.1）
        let open: Vec<String> = self
            .open_tools
            .iter()
            .filter(|(_, owner)| owner.as_str() == turn.as_str())
            .map(|(id, _)| id.clone())
            .collect();
        for id in open {
            self.close_unsettled(edit, &id, Some(time.ended_at));
        }
        self.control(edit);
        edit.state(state::keys::ADMISSION, self.admission());
    }
}

#[cfg(test)]
#[path = "projection_lists_tests.rs"]
mod tests;
