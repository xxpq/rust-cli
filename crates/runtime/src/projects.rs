//! ← 项目页与 `/resume` 页的内核部分（修复线 F29，docs/specs/rust-cli/tui-panels.md §2、tui-manage.md §10）：存储的
//! 查询合并本进程里打开着的会话的状态与每个目录的分支；改名、删除与只读快照按“打开着的会话由 actor 管”分派。

use rust_cli_kernel_api::env_files::GIT_DIR;
use std::collections::HashMap;

use rust_cli_kernel_api::env::{EnvPath, ReadFile, WorkspaceRef};
use rust_cli_kernel_api::env_files::{FileKind, StatFile};
use rust_cli_kernel_api::env_git::{gitdir_pointer, marks_git_root};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::machine::Files;
use rust_cli_kernel_api::panels::{
    ProjectSession, ProjectSessions, SessionActivity, SessionPresence,
};
use rust_cli_kernel_api::peers::PeerStatus;
use rust_cli_kernel_api::session_api::{Command, CommandEnvelope, EndReason, SessionView};
use rust_cli_kernel_api::store::{SessionQuery, SessionRecord, StoreError};

use super::workspace_sessions::{activity, last_reply};
use super::*;

/// 盘符 `C:` 的长度。
const DRIVE_LEN: usize = rust_cli_base::path::DRIVE_PREFIX_LEN;

/// 路径的上一层（按字符串，两种分隔符都认）；已经是根时为 None。
fn parent(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let index = trimmed.rfind(['/', '\\'])?;
    Some(if index == 0 {
        &trimmed[..1]
    } else {
        &trimmed[..index]
    })
}

fn join(dir: &str, name: &str) -> String {
    let separator = if dir.contains('\\') && !dir.contains('/') {
        '\\'
    } else {
        '/'
    };
    format!("{}{separator}{name}", dir.trim_end_matches(['/', '\\']))
}

/// 按段去掉 `.`、`..` 回退一段（不碰文件系统；`gitdir:` 给的相对路径相对 `.git` 所在的目录）。
fn normalize(path: &str) -> String {
    let separator = if path.contains('\\') && !path.contains('/') {
        '\\'
    } else {
        '/'
    };
    let leading = path.starts_with(['/', '\\']);
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join(&separator.to_string());
    if leading {
        format!("{separator}{joined}")
    } else {
        joined
    }
}

/// 绝对路径（`/…`、`\…`、`C:…`）。
fn absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with(['/', '\\'])
        || (bytes.len() >= DRIVE_LEN && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

/// 目录现在的 git 分支（tui-panels.md §2.1、D11）：从目录向上找 `.git`；是文件时按 `gitdir:` 找；读 `HEAD` 的
/// `ref: refs/heads/<名字>`。只经执行环境的 `stat` / `read_file`，不跑 git。UNC 路径不读（同 workspace-trust.md §2.1）。
pub(crate) async fn branch(
    files: &dyn Files,
    environment: &EnvironmentId,
    root: &EnvPath,
) -> Option<String> {
    let workspace = WorkspaceRef {
        environment: environment.clone(),
        root: root.clone(),
    };
    let read = |path: String| {
        let workspace = workspace.clone();
        async move {
            let content = files
                .read_file(
                    &workspace,
                    ReadFile {
                        path: EnvPath::new(path),
                    },
                )
                .await
                .ok()?;
            Some(String::from_utf8_lossy(&content.bytes).trim().to_owned())
        }
    };
    let mut dir = Some(root.as_str());
    while let Some(current) = dir {
        let dot_git = join(current, GIT_DIR);
        let stat = files
            .stat(
                &workspace,
                StatFile {
                    path: EnvPath::new(dot_git.as_str()),
                },
            )
            .await
            .ok()
            .flatten();
        // 缺陷修复（F58）：以前 `.git` 是 FIFO、socket、设备时也当文件读，读不到就没有分支；只认
        // 目录与文件，别的种类继续往上。判定与 `gitdir:` 的解析（含 UNC 的拒绝）改用 kernel-api（唯一定义）。
        let head = match stat
            .map(|stat| stat.kind)
            .filter(|kind| marks_git_root(*kind))
        {
            Some(FileKind::Directory) => join(&dot_git, "HEAD"),
            Some(_) => {
                let pointer = read(dot_git).await?;
                let gitdir = gitdir_pointer(&pointer)?;
                let gitdir = if absolute(gitdir) {
                    gitdir.to_owned()
                } else {
                    normalize(&join(current, gitdir))
                };
                join(&gitdir, "HEAD")
            }
            None => {
                dir = parent(current);
                continue;
            }
        };
        let head = read(head).await?;
        return head
            .strip_prefix("ref:")
            .map(str::trim)
            .and_then(|reference| reference.strip_prefix("refs/heads/"))
            .map(str::to_owned);
    }
    None
}

impl LocalKernel {
    /// 把登记处合进行（tui-cockpit.md §1）：别的进程的状态只有这里有；本进程的时刻与现场也从自己的登记读。
    /// 跨会话关闭时登记是空的，行上只有 actor 的 view 给的状态。
    async fn join_registry(&self, sessions: &mut [ProjectSession]) {
        let port = &self.peers.port;
        let others = port.peers().await.into_iter().map(|record| (false, record));
        let own = port.own().await.into_iter().map(|record| (true, record));
        let mut live: HashMap<_, _> = others
            .chain(own)
            .filter_map(|(here, record)| Some((record.session.clone()?, (here, record))))
            .collect();
        for session in sessions {
            let Some((here, record)) = live.remove(&session.record.id) else {
                continue;
            };
            if !here {
                session.activity = record.status.map(|status| match status {
                    PeerStatus::Waiting => SessionActivity::NeedsInput,
                    PeerStatus::Busy => SessionActivity::Working,
                    PeerStatus::Idle => SessionActivity::Ready,
                });
            }
            session.presence = Some(SessionPresence {
                here,
                address: record.address,
                since_ms: record.status_since_ms,
                presence: record.presence,
                tmux: record.tmux,
            });
        }
    }

    /// 打开着的会话（不在会话表里时为 None）。加载中、关闭中的先等它稳定（session-lifecycle.md §3.3.1，F33）：
    /// 关闭中的 actor 不再接命令，关完之后才能直接读写存储。
    pub(crate) async fn open(&self, session: &SessionId) -> Option<Arc<Session>> {
        self.settled(session).await
    }

    pub(crate) async fn projects(
        &self,
        query: SessionQuery,
    ) -> Result<ProjectSessions, KernelError> {
        let environment = query.environment.clone();
        let root = query.root.clone();
        let page = self.ports.store.query_sessions(query).await?;
        // 先在锁里取出句柄，放锁后再逐个问（view 要等 actor 回复）。只合并打开着的：加载中、关闭中的按库里的行。
        let open: Vec<Arc<Session>> = self
            .sessions()
            .values()
            .filter_map(crate::table::Slot::open)
            .cloned()
            .collect();
        let mut live = HashMap::new();
        for session in open {
            let Ok(view) = session.view().await else {
                continue;
            };
            if view.meta.kind == SessionKind::Subagent
                || view.meta.workspace.environment != environment
                || root
                    .as_ref()
                    .is_some_and(|root| &view.meta.workspace.root != root)
            {
                continue;
            }
            live.insert(view.meta.id.clone(), view);
        }
        let mut sessions: Vec<ProjectSession> = page
            .sessions
            .into_iter()
            .map(|record| {
                let view = live.remove(&record.id);
                ProjectSession {
                    activity: view.as_ref().map(activity),
                    last_reply: view.as_ref().and_then(last_reply),
                    branch: None,
                    presence: None,
                    record,
                }
            })
            .collect();
        // 打开着、但不在这一页里的（超出上限）也列出来。
        for (id, view) in live {
            sessions.push(ProjectSession {
                activity: Some(activity(&view)),
                last_reply: last_reply(&view),
                branch: None,
                presence: None,
                record: SessionRecord {
                    id,
                    title: view.meta.title.clone(),
                    root: view.meta.workspace.root.clone(),
                    created_at_ms: view.meta.created_at_ms,
                    updated_at_ms: view.meta.created_at_ms,
                    first_prompt: None,
                },
            });
        }
        self.join_registry(&mut sessions).await;
        // 每个目录读一次分支。
        let mut branches: HashMap<EnvPath, Option<String>> = HashMap::new();
        for session in &mut sessions {
            let root = session.record.root.clone();
            if !branches.contains_key(&root) {
                let found = branch(self.machine().files().as_ref(), &environment, &root).await;
                branches.insert(root.clone(), found);
            }
            session.branch = branches.get(&root).cloned().flatten();
        }
        Ok(ProjectSessions {
            sessions,
            more: page.more,
        })
    }

    /// 打开着的会话经它的 actor（唯一 owner，`TitleChanged` 更新视图）；否则直接写存储。
    pub(crate) async fn rename(
        &self,
        session: &SessionId,
        title: String,
    ) -> Result<(), KernelError> {
        let _slot = match self.claim(session).await {
            table::Claim::Load(slot) => slot,
            table::Claim::Open(open) => {
                let envelope = CommandEnvelope {
                    id: self.ports.ids.command(),
                    command: Command::Rename { title },
                };
                return match open.send(envelope).await {
                    rust_cli_kernel_api::session_api::Ack::Rejected { reason, .. }
                    | rust_cli_kernel_api::session_api::Ack::OutcomeUnknown { reason } => {
                        Err(KernelError::Failed { message: reason })
                    }
                    _ => Ok(()),
                };
            }
        };
        self.ports
            .store
            .rename(session, &title)
            .await
            .map_err(KernelError::from)
    }

    /// 删除（docs/specs/rust-cli/session-facts.md §5）：占住会话表的条目再软删除。打开着的先关掉（中止在跑的回合、不再
    /// 记录它，同退出；执行环境忘掉它的后台命令），关完再占；删除期间打开它的调用等着条目放开，冷加载看到 NotFound。
    ///
    /// 修改（v5 重做 KERNEL-B）：以前打开着的会话拒绝删除（`The session is open in this window.`），v5 要能删打开着的
    /// 会话（02 §6 的 `session.delete`）。占住条目再删是为了两个竞态：关闭之后 actor 不再提交（草稿的首条输入不会在删除
    /// 之后落库）；关闭之后、软删除之前不会有人在这个会话上起新的 actor。TUI 不删现在的会话（它在界面上挡，tui-manage.md
    /// §10 的 ⌃d），行为不变。云端的会话库以前就是先关再删（会话策略的 `DeleteOpen::CloseFirst`）；本机也这样之后两种部署
    /// 不再不同，这一项策略随之去掉（architecture.md §3）。已删除的会话再删一次由会话库自己处理（幂等），这里不先问。
    pub(crate) async fn delete(&self, session: &SessionId) -> Result<(), KernelError> {
        let mut closed = false;
        let _held = loop {
            match self.claim(session).await {
                crate::table::Claim::Open(open) => {
                    open.close(EndReason::Other).await;
                    closed = true;
                }
                crate::table::Claim::Load(held) => break held,
            }
        };
        match self.ports.store.delete(session).await {
            // 关掉的是草稿：它没有落库（session-facts.md §4），关掉就是删掉。
            Err(StoreError::NotFound { .. }) if closed => Ok(()),
            result => result.map_err(KernelError::from),
        }
    }

    /// 回退文件的预览（session-lifecycle.md §10.53）：打开着的问它的 actor（与回退同一份 checkpoint 清单），否则冷加载，按存储给的消息与
    /// checkpoint 算（同打开之后 actor 的起始状态）；不启动 actor、不跑 SessionStart。
    pub(crate) async fn preview(
        &self,
        session: &SessionId,
        target: &rust_cli_kernel_api::ids::MessageId,
    ) -> Result<Option<rust_cli_kernel_api::rewind_preview::RewindPreview>, KernelError> {
        if let Some(open) = self.open(session).await {
            return open.rewind_preview(target).await;
        }
        let loaded = self.ports.store.load(session).await?;
        Ok(rust_cli_session::rewind_preview(
            self.ports.artifacts.as_ref(),
            self.ports.environments.as_ref(),
            &loaded.meta.workspace,
            &loaded.messages,
            &loaded.checkpoints,
            target,
        )
        .await)
    }

    /// 一个回合的文件 diff（session-lifecycle.md §10.45）：打开着的问它的 actor，否则冷加载，按存储给的消息、回合记录与 checkpoint 算（同打开之后
    /// actor 的起始状态）；不启动 actor、不跑 SessionStart。
    pub(crate) async fn file_diffs(
        &self,
        session: &SessionId,
        turn: &rust_cli_kernel_api::ids::TurnId,
    ) -> Result<Vec<rust_cli_kernel_api::session_api::TurnFileDiff>, KernelError> {
        if let Some(open) = self.open(session).await {
            return open.turn_file_diffs(turn).await;
        }
        let loaded = self.ports.store.load(session).await?;
        Ok(rust_cli_session::turn_file_diffs(self.ports.artifacts.as_ref(), &loaded, turn).await)
    }

    /// 只读快照：打开着的问 actor，否则冷加载（不启动 actor、不跑 SessionStart）。
    pub(crate) async fn snapshot(&self, session: &SessionId) -> Result<SessionView, KernelError> {
        if let Some(open) = self.open(session).await {
            return open.view().await;
        }
        let loaded = self.ports.store.load(session).await?;
        Ok(self.cold_view(session, loaded).await)
    }

    /// 前端冷读只读最后一段的视图（node-store.md §5.6）：打开着的问 actor（整段，子代理读库，同以前）；否则只读最后 `records` 条存储消息（补到回合开头），
    /// 视图的组法同 [`Self::snapshot`]。
    pub(crate) async fn tail(
        &self,
        session: &SessionId,
        records: usize,
    ) -> Result<rust_cli_kernel_api::session_api::SessionTail, KernelError> {
        if let Some(open) = self.open(session).await {
            let view = open.view().await?;
            // 库里没有这个会话（没有独立记录的子会话）时没有子代理（同前端冷读以前的读法）
            let subagents = match self.ports.store.subagents(session).await {
                Err(rust_cli_kernel_api::store::StoreError::NotFound { .. }) => Vec::new(),
                read => read?,
            };
            return Ok(rust_cli_kernel_api::session_api::SessionTail {
                view,
                subagents,
                earlier: false,
            });
        }
        let span = rust_cli_kernel_api::store::HistorySpan::Before {
            floor: None,
            records,
        };
        let range = self.ports.store.range(session, span).await?;
        Ok(rust_cli_kernel_api::session_api::SessionTail {
            view: self.cold_view(session, range.loaded).await,
            subagents: range.subagents,
            earlier: range.earlier,
        })
    }

    /// 冷加载的结果组成视图：模型与模式的缺省、上下文用量、任务清单（整段与范围读同一个组法）。
    async fn cold_view(
        &self,
        session: &SessionId,
        loaded: rust_cli_kernel_api::store::LoadedSession,
    ) -> SessionView {
        let execution = loaded.mode.map_or_else(
            || self.defaults.mode.into(),
            |mode| rust_cli_kernel_api::permission::ExecutionState::from_parts(mode, loaded.plan),
        );
        let selection = loaded
            .model
            .unwrap_or_else(|| self.defaults.selection.current());
        SessionView {
            plan: execution.plan,
            // 修 bug（PROJ-GAPS-2，§10.49）：以前冷快照不算上下文用量，由它建的投影首个快照没有窗口与已用 token。打开之后会话 actor 按
            // 模型侧的历史与会话的模型经同一个压缩模块算（`context_occupancy`），这里用同样的两样，所以冷读与打开之后同值。
            context_usage: self.compactor.context_usage(&loaded.messages, &selection),
            // 修 bug（RUST-SESSION，session-lifecycle.md §10.1、§10.13）：以前冷快照丢了回合记录，前端冷读出的行都不属于任何
            // 回合，与活会话的行对不上。回合记录与消息同一次读出，原样带上。
            turns: loaded.turns,
            // 输入记录同样随快照（§10.14）：冷读的行与活会话带同样的附件。
            inputs: loaded.inputs,
            // 修 bug（ROW-SOURCE，§10.30）：输入的消息与它的命令也要随快照，否则冷读的用户行给不出与活会话相同的命令。
            sources: loaded.sources,
            // 修 bug（PROJ-GAPS，§10.33）：工具结果的输出头部也要随快照，否则由冷快照建的投影只有给模型的文字。
            output_heads: loaded.output_heads,
            // 修 bug（TOOL-TIME，§10.37）：工具调用的起止也要随快照，否则由冷快照建的投影的工具行没有时刻（子代理的子会话只经冷快照建）。
            tool_times: loaded.tool_times,
            // ExitPlanMode 拿给用户批准的计划（PROJ-FACTS，§10.43）：由冷快照建的投影里，计划行与活会话同样带计划文字。
            plans: loaded.plans,
            // 撤销过文件改动的回合（session-lifecycle.md §10.54）：与打开着的会话 actor 同一个换法
            reverted: rust_cli_session::reverted_prompts(&loaded.reverted),
            // 修 bug（SMALL-6，agents.md §7.2，K-S9）：以前冷快照不带任务清单，冷读的 `state.plan` 一直是空的。与打开之后会话 actor 的
            // 视图同一个函数读清单文件，所以冷读与活会话同值。
            tasks: self.agents.task_list(session).await,
            ..SessionView::new(
                loaded.meta, // 显示用的历史：压缩点之前只供显示的消息接上模型侧的历史（工作线 F102，compact.md §5.1）。
                // 缺陷修复（MEM-V5，performance.md §13）：以前 `concat` 把整份历史（带工具的全文）复制一份，打开长会话时与读出的那份同时在内存里；
                // 两段都是这里拥有的，接上即可
                loaded.earlier.into_iter().chain(loaded.messages).collect(),
                selection,
                execution.mode,
            )
        }
    }
}

#[cfg(test)]
#[path = "projects_tests.rs"]
mod tests;
