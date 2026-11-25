//! 内核会话 API 的本地实现（[`Kernel`]）：建会话、打开会话（冷加载）、列出会话。

use super::*;
use rust_cli_kernel_api::permission::ExecutionState;
use rust_cli_kernel_api::session_api::Persistence;
use rust_cli_kernel_api::session_preferences::OpenStart;
use rust_cli_kernel_api::session_prompt::SystemPin;
use rust_cli_kernel_api::store::entry_kinds;
use rust_cli_kernel_api::working_dirs;
use rust_cli_session::PromptPin;

#[async_trait]
impl Kernel for LocalKernel {
    async fn create_session(&self, request: CreateSession) -> Result<SessionId, KernelError> {
        self.create_session_as(request, StartSource::Startup).await
    }

    /// 旧会话的询问（agents.md §4.1.2，工作线 F72）。
    fn detached_asks(
        &self,
    ) -> futures_util::stream::BoxStream<'static, rust_cli_kernel_api::orchestration::DetachedEvent>
    {
        self.detached.subscribe()
    }

    async fn resolve_detached(
        &self,
        interaction: &rust_cli_kernel_api::ids::InteractionId,
        answer: rust_cli_kernel_api::orchestration::Answer,
    ) -> Result<(), KernelError> {
        self.detached
            .resolve(interaction, answer, |id| async move {
                use rust_cli_kernel_api::session_policy::DetachedAnswer;
                if self.policy.detached_answer == DetachedAnswer::Direct {
                    return Ok(None);
                }
                match self.claim(&id).await {
                    table::Claim::Load(slot) => Ok(Some(slot)),
                    table::Claim::Open(_) => Err(KernelError::Failed {
                        message: format!("session already open: {id}"),
                    }),
                }
            })
            .await
    }

    async fn open_session(
        &self,
        session: &SessionId,
    ) -> Result<Arc<dyn SessionHandle>, KernelError> {
        self.open_session_as(session, StartSource::Resume).await
    }

    async fn create_session_as(
        &self,
        request: CreateSession,
        start: StartSource,
    ) -> Result<SessionId, KernelError> {
        // 工作线 F140（performance.md §12.3）：先等新会话的缺省模型定下。TUI 不等目录就画第一帧，缺省模型在后台选；
        // 这里是唯一的等待点，所以回合都在它定下之后（任务工具的开关、旁路模型的兜底读的都是定下的值）。
        self.defaults.selection.settled().await;
        // 调用方给的 id（session-facts.md §6，02 §11.2 F）：已经有这个会话时回原 id，不重写会话行、不起第二个 actor；
        // 占住会话表的条目直到 actor 起来（`start` 换掉它），同 id 并发的建与打开等它。内核生成的 id 是新的，不查库，
        // 同样占住条目：创建和冷打开共用会话表，`start` 只换调用方自己登记的占位（否则并发打开可能在创建提交后另起一个
        // actor）。
        let (id, _claimed) = match request.id.clone() {
            Some(id) => match self.claim_new(&id, &request.workspace).await? {
                Some(claimed) => (id, claimed),
                None => return Ok(id),
            },
            None => {
                let id = self.ports.ids.session();
                match self.claim(&id).await {
                    table::Claim::Load(claimed) => (id, claimed),
                    table::Claim::Open(_) => {
                        return Err(KernelError::Failed {
                            message: format!("session already open: {id}"),
                        });
                    }
                }
            }
        };
        // 起始的附加工作目录（session-dirs.md §3.1）：内核缺省接上调用方带的，相对路径与已在工作区根里的不收。
        let additional_directories = working_dirs::admit(
            &request.workspace.root,
            self.defaults
                .additional_directories
                .iter()
                .cloned()
                .chain(request.additional_directories),
        );
        let meta = SessionMeta {
            id: id.clone(),
            kind: SessionKind::Interactive,
            parent: None,
            workspace: request.workspace,
            title: String::new(),
            // 顶层会话的链路 ID 取会话 ID；子会话、子代理继承父会话的链路 ID。
            trace: TraceId::new(id.as_str()),
            created_at_ms: self.ports.clock.now_ms(),
        };
        // 执行状态（修复线 F20）：单值的 `plan` 是“plan 开、default”，另记的 `plan` 打开 plan。
        let execution = ExecutionState::from_parts(request.mode, request.plan);
        // 会话钉住的 system 输入随初始事实落库（context.md §15.3）：之后冷打开（续写、serve 的下一轮）沿用它。
        let prompt = PromptPin::created(self.prompt.clone());
        let config = vec![
            Fact::ModelSelected {
                selection: request.selection.clone(),
            },
            Fact::ExecutionChanged { state: execution },
            prompt.fact(),
        ];
        let mut state = SessionState::created(meta.clone(), request.selection, execution, start);
        state.additional_directories = additional_directories;
        state.preferences = request.preferences;
        state.prompt = prompt;
        match request.persistence {
            // 建会话与初始事实同一事务（pg-store.md §3）。云端的会话规则在判决前现读（assemble::gate）。
            Persistence::Immediate => {
                let batch = FactBatch {
                    command: None,
                    facts: config,
                    turn: None,
                };
                self.ports.store.create(meta, batch).await?;
            }
            // 草稿（session-facts.md §4）：会话行与配置交给会话 actor，随第一批带消息的提交落库。
            Persistence::Deferred => {
                state.draft = Some([vec![Fact::SessionCreated { meta }], config].concat());
            }
        }
        self.start(state, Opened::Created);
        Ok(id)
    }

    async fn open_session_as(
        &self,
        session: &SessionId,
        start: StartSource,
    ) -> Result<Arc<dyn SessionHandle>, KernelError> {
        self.open_session_with(session, start, OpenStart::default())
            .await
    }

    async fn open_session_with(
        &self,
        session: &SessionId,
        start: StartSource,
        given: OpenStart,
    ) -> Result<Arc<dyn SessionHandle>, KernelError> {
        // 同 `create_session_as`：开会话也先等缺省模型定下。
        self.defaults.selection.settled().await;
        // 修复（F33，session-lifecycle.md §3.3.1）：以前表里有这个会话就直接给出句柄，它正在关闭时（actor 已退出、
        // 还没移出表）打开拿到的是死句柄，`view()` 报 `the session is closed`；两个并发的打开还会各自冷加载。现在
        // 关闭中、加载中的等它换掉；没有条目时登记 Loading，由这个调用冷加载（失败或被丢下时守卫移出 Loading）。
        let _loading = match self.claim(session).await {
            table::Claim::Open(open) => return Ok(open),
            table::Claim::Load(loading) => loading,
        };
        let loaded = self.ports.store.load(session).await?;
        // Node 群协作会话（`permission.scope = session`）的规则只在会话行里：这个会话的判决用它代替本机项目
        // 规则集（修复线 F2，shell-permissions.md §5）。“总是允许”与改模式由存储按同一作用域落库。条目是这个调用
        // 的 Loading，期间不会有同一个会话的关闭插进来 `release` 它。
        let rules = self.ports.store.session_rules(session).await?;
        // 输入账本里还没提升的输入（pg-store.md §4）：会话 actor 启动时按接纳顺序放回队列。不记账的账本没有。
        let pending_inputs = self.journal.pending(session).await?;
        // 会话之外按会话保存的状态（判决链的会话规则、“先读后改”记录）在启动 actor 之前恢复（table.rs 的
        // `SessionScoped`，关闭时在同一处释放）。
        self.closer
            .scoped
            .restored(
                session,
                &loaded,
                rules.as_ref(),
                self.machine().files().as_ref(),
            )
            .await;
        let mut state = SessionState::from_loaded(
            loaded,
            &self.defaults.selection.current(),
            self.defaults.mode,
            start,
        );
        state.pending_inputs = pending_inputs;
        // fork 出的会话第一次打开时取走父会话记下的起始状态（附加目录；侧聊另有会话层规则的快照，session-lifecycle.md §10.58）
        let inherited = self.forks.take(session);
        state.session_rules = inherited.rules;
        // 附加工作目录不落库（session-dirs.md §3.1、§12）：冷打开的会话起始是内核缺省，接上 fork 时父会话的（fork 出的会话第一次打开时
        // 取走）与调用方带的（App 里是 Host 给的）。已经打开着的会话走不到这里：带的目录不起作用（运行实例存在时不刷新）。
        state.additional_directories = working_dirs::admit(
            &state.meta.workspace.root,
            self.defaults
                .additional_directories
                .iter()
                .cloned()
                .chain(inherited.directories)
                .chain(given.directories),
        );
        // 会话偏好不落库（app-prefs.md §4.2）：冷打开的会话用调用方给的（App 里是 Host 给的），没给时什么都不关。
        state.preferences = given.preferences;
        // 钉住的 system 输入沿用库里记下的（续写换了参数也不变，直到压缩，context.md §15.4）；附件那一层是内核现在的。
        let recorded = self
            .ports
            .store
            .entries(session, entry_kinds::SYSTEM_PIN)
            .await?;
        state.prompt = PromptPin::loaded(SystemPin::recorded(&recorded), self.prompt.clone());
        Ok(self.start(state, Opened::Loaded))
    }

    async fn list_sessions(
        &self,
        filter: SessionFilter,
    ) -> Result<Vec<SessionSummary>, KernelError> {
        self.ports
            .store
            .list(filter)
            .await
            .map_err(KernelError::from)
    }

    /// `/context`（context-usage.md §2）：有会话时按会话 actor 的当前视图（历史、模型、模式），只读。
    async fn context_usage(
        &self,
        query: rust_cli_kernel_api::context_usage::ContextUsageQuery,
    ) -> Result<rust_cli_kernel_api::context_usage::ContextUsage, KernelError> {
        // 没有会话时附加目录是内核缺省，有会话时是会话现在的（session-dirs.md §3.3）。
        let Some(session) = &query.session else {
            let additional = self.defaults.additional_directories.clone();
            return self
                .usage
                .analyze(query, (&[], &additional), &self.prompt)
                .await;
        };
        let view = self.open_session(session).await?.view().await?;
        let query = rust_cli_kernel_api::context_usage::ContextUsageQuery {
            selection: view.selection,
            // 判决用的模式（plan 开时是 plan，修复线 F20）。
            mode: ExecutionState::from_parts(view.mode, view.plan).effective(),
            ..query
        };
        self.usage
            .analyze(
                query,
                (&view.messages, &view.additional_directories),
                &self.prompt,
            )
            .await
    }

    /// `@` 补全的文件清单（tui-input.md §1.2）：执行环境从工作区根遍历（遵守忽略规则）。
    async fn workspace_files(
        &self,
        workspace: &rust_cli_kernel_api::env::WorkspaceRef,
    ) -> Result<Vec<String>, KernelError> {
        let request = rust_cli_kernel_api::env_files::ListFiles {
            cwd: workspace.root.clone(),
        };
        self.machine()
            .search()
            .list_files(workspace, request)
            .await
            .map(|list| list.files)
            .map_err(|error| KernelError::Failed {
                message: error.to_string(),
            })
    }

    /// `-p` 的 stream-json 的 system/init：会话可用的工具、agent 与 skill（headless.md §3.3）。
    async fn session_catalog(
        &self,
        query: rust_cli_kernel_api::catalog::CatalogQuery,
    ) -> Result<rust_cli_kernel_api::catalog::SessionCatalog, KernelError> {
        Ok(catalog::catalog(
            &self.usage,
            &query,
            (&self.defaults.additional_directories, self.ext.plugins()),
        )
        .await)
    }

    /// 工作区的 agent 类型（agents.md §8 的 PT 增量）：不要会话。
    async fn agent_definitions(
        &self,
        workspace: &rust_cli_kernel_api::env::WorkspaceRef,
    ) -> Vec<rust_cli_kernel_api::catalog::AgentInfo> {
        catalog::agents(
            &self.usage,
            workspace,
            &self.defaults.additional_directories,
        )
        .await
    }

    /// `/mcp`、`/status`：ext 的连接管理给出的状态（tui-display.md §3.4）。
    async fn mcp_servers(&self) -> Vec<rust_cli_kernel_api::manage::McpServerInfo> {
        self.ext.mcp().server_infos_with_auth().await
    }

    /// 远程服务器的交互授权（docs/specs/rust-cli/ext.md §3.12.6、§3.12.9）：经 ext 的会话级连接表（内核级与会话级的服务器都认）。
    async fn mcp_sign_in(
        &self,
        server: String,
    ) -> Result<rust_cli_kernel_api::manage::McpSignIn, KernelError> {
        self.ext
            .mcp_sessions()
            .sign_in(&server)
            .await
            .map_err(|message| KernelError::Failed { message })
    }

    /// 一次交互授权的结局（docs/specs/rust-cli/ext.md §3.12.14）。
    async fn mcp_sign_in_outcome(
        &self,
        server: String,
        url: String,
    ) -> Result<rust_cli_kernel_api::manage::McpSignInOutcome, KernelError> {
        self.ext
            .mcp_sessions()
            .sign_in_outcome(&server, &url)
            .await
            .map_err(|message| KernelError::Failed { message })
    }

    /// 用 OAuth 的服务器的授权状态（docs/specs/rust-cli/ext.md §3.12.9）。
    async fn mcp_oauth_servers(&self) -> Vec<rust_cli_kernel_api::manage::McpOAuthServer> {
        self.ext.mcp_sessions().oauth_servers().await
    }

    /// Sign out 一个远程服务器（docs/specs/rust-cli/ext.md §3.12.6、§3.12.14）：经 ext 的会话级连接表。
    async fn mcp_sign_out(
        &self,
        server: String,
    ) -> Result<Option<rust_cli_kernel_api::manage::McpServerInfo>, KernelError> {
        self.ext
            .mcp_sessions()
            .sign_out(&server)
            .await
            .map_err(|message| KernelError::Failed { message })
    }

    /// `/plan open`：计划文件的位置（与 ExitPlanMode 同一个来源）与它有没有内容（tui-manage.md §8）。
    async fn plan_file(
        &self,
        query: rust_cli_kernel_api::manage::PlanFileQuery,
    ) -> Result<rust_cli_kernel_api::manage::PlanFile, KernelError> {
        directory::plan_file(
            self.machine().info().as_ref(),
            self.machine().files().as_ref(),
            &query,
        )
        .await
    }

    /// `/cd <path>`：检查路径，有会话时经会话 actor 换工作目录（tui-manage.md §7.1）。
    async fn change_directory(
        &self,
        request: rust_cli_kernel_api::manage::ChangeDirectory,
    ) -> Result<rust_cli_kernel_api::manage::DirectoryChanged, KernelError> {
        directory::change(self, request).await
    }

    /// `/add-dir <path>` 的确认框之前：只检查（permission-updates.md §6.1）。
    async fn check_directory(
        &self,
        request: rust_cli_kernel_api::manage::AddDirectory,
    ) -> Result<rust_cli_kernel_api::manage::DirectoryChecked, KernelError> {
        dirs::check(self, request).await
    }

    /// `/add-dir <path>`：检查路径，有会话时经会话 actor 加进去（session-dirs.md §5.1）。
    async fn add_directory(
        &self,
        request: rust_cli_kernel_api::manage::AddDirectory,
    ) -> Result<rust_cli_kernel_api::manage::DirectoryAdded, KernelError> {
        dirs::add(self, request).await
    }

    /// `/mcp reconnect|enable|disable`：经 ext 的连接管理（tui-manage.md §6.1）。
    async fn mcp_control(
        &self,
        control: rust_cli_kernel_api::manage::McpControl,
    ) -> Result<Vec<rust_cli_kernel_api::manage::McpServerInfo>, KernelError> {
        Ok(self
            .ext
            .mcp()
            .control(control.action, &control.servers)
            .await)
    }

    /// 存储的只读查询与前端记录（审查 K7）：同一个存储适配器，不经会话 actor（这些都不是会话状态）。
    fn queries(&self) -> Arc<dyn rust_cli_kernel_api::store::StoreQueries> {
        self.ports.store.clone()
    }

    /// 活动统计（usage.md §5）：今天与本地日的偏移取自注入的时钟，聚合在存储。
    async fn activity(
        &self,
        range: rust_cli_kernel_api::metering::ActivityRange,
    ) -> Result<rust_cli_kernel_api::metering::ActivityStats, KernelError> {
        let clock = &self.ports.clock;
        let query = rust_cli_kernel_api::metering::ActivityQuery {
            range,
            today: clock.today(),
            utc_offset_minutes: clock.utc_offset_minutes(clock.now_ms()),
        };
        self.ports
            .store
            .activity(query)
            .await
            .map_err(KernelError::from)
    }

    /// ← 会话面板：库里的会话合并本进程里打开着的会话的实时状态（tui-panels.md §2.1）。
    async fn workspace_sessions(
        &self,
        filter: SessionFilter,
    ) -> Result<Vec<rust_cli_kernel_api::panels::WorkspaceSession>, KernelError> {
        self.overview(filter).await
    }

    /// ← 项目页与 `/resume` 页（修复线 F29，tui-panels.md §2.1）。
    async fn project_sessions(
        &self,
        query: rust_cli_kernel_api::store::SessionQuery,
    ) -> Result<rust_cli_kernel_api::panels::ProjectSessions, KernelError> {
        self.projects(query).await
    }

    async fn set_presence(
        &self,
        session: &SessionId,
        presence: rust_cli_kernel_api::peers::PeerPresence,
    ) {
        self.peers.port.set_presence(session, presence).await;
    }

    async fn stop_peer(
        &self,
        address: &rust_cli_kernel_api::peers::PeerAddress,
    ) -> Result<(), KernelError> {
        self.peers
            .port
            .stop(address)
            .await
            .map_err(|error| KernelError::Failed {
                message: error.to_string(),
            })
    }

    async fn rename_session(&self, session: &SessionId, title: String) -> Result<(), KernelError> {
        self.rename(session, title).await
    }

    async fn delete_session(&self, session: &SessionId) -> Result<(), KernelError> {
        self.delete(session).await
    }

    async fn deletion_preview(
        &self,
        root: &SessionId,
        workspace: &rust_cli_kernel_api::env::WorkspaceRef,
    ) -> Result<rust_cli_kernel_api::deletion::DeletionPreview, KernelError> {
        LocalKernel::deletion_preview(self, root, workspace).await
    }

    async fn delete_permanently(
        &self,
        root: &SessionId,
        workspace: &rust_cli_kernel_api::env::WorkspaceRef,
        revision: &str,
        operation: &str,
    ) -> Result<rust_cli_kernel_api::deletion::DeletionResult, KernelError> {
        LocalKernel::delete_permanently(self, root, workspace, revision, operation).await
    }

    async fn session_snapshot(
        &self,
        session: &SessionId,
    ) -> Result<rust_cli_kernel_api::session_api::SessionView, KernelError> {
        self.snapshot(session).await
    }

    async fn session_tail(
        &self,
        session: &SessionId,
        records: usize,
    ) -> Result<rust_cli_kernel_api::session_api::SessionTail, KernelError> {
        self.tail(session, records).await
    }

    async fn input_receipt(
        &self,
        session: &SessionId,
        command: &rust_cli_kernel_api::ids::CommandId,
    ) -> Result<Option<rust_cli_kernel_api::session_api::Ack>, KernelError> {
        Ok(self
            .journal
            .input(session, command)
            .await?
            .map(|input| input.receipt))
    }

    async fn rewind_preview(
        &self,
        session: &SessionId,
        target: &rust_cli_kernel_api::ids::MessageId,
    ) -> Result<Option<rust_cli_kernel_api::rewind_preview::RewindPreview>, KernelError> {
        self.preview(session, target).await
    }

    async fn turn_file_diffs(
        &self,
        session: &SessionId,
        turn: &rust_cli_kernel_api::ids::TurnId,
    ) -> Result<Vec<rust_cli_kernel_api::session_api::TurnFileDiff>, KernelError> {
        self.file_diffs(session, turn).await
    }

    /// `/hooks`：hooks 引擎当前快照里的配置（tui-display.md §3.5）。
    async fn hooks(&self) -> Vec<rust_cli_kernel_api::manage::HookEntry> {
        self.hooks.entries()
    }

    /// `/diff` 的未提交改动：执行环境里 git 的原始输出（tui-manage.md §4.1）。
    async fn uncommitted_diff(
        &self,
        workspace: &rust_cli_kernel_api::env::WorkspaceRef,
    ) -> Result<rust_cli_kernel_api::env_git::GitDiff, KernelError> {
        let request = rust_cli_kernel_api::env_git::GitDiffRequest {
            cwd: workspace.root.clone(),
            numstat_only: false,
        };
        // `/diff` 看会话主 env 上的仓库（deployment.md §10）。
        self.machine()
            .git()
            .git_diff(workspace, request)
            .await
            .map_err(|error| KernelError::Failed {
                message: error.to_string(),
            })
    }

    /// 状态栏第 2 行（tui-display.md §2.6）：分支同任务列表的读法（`projects::branch`），行数只问 numstat（不拼整份 patch）。
    async fn working_tree(
        &self,
        workspace: &rust_cli_kernel_api::env::WorkspaceRef,
    ) -> Result<rust_cli_kernel_api::env_git::WorkingTree, KernelError> {
        use rust_cli_kernel_api::env_git::{GitDiff, GitDiffRequest, WorkingTree, numstat_totals};
        let machine = self.machine();
        let request = GitDiffRequest {
            cwd: workspace.root.clone(),
            numstat_only: true,
        };
        let (branch, diff) = futures_util::join!(
            crate::projects::branch(
                machine.files().as_ref(),
                &workspace.environment,
                &workspace.root
            ),
            machine.git().git_diff(workspace, request),
        );
        // git 跑不起来（没装、出错）时分支照样有：分支只读文件，行数才要 git；状态栏只少了行数（tui-display.md §2.6）。
        let (added, removed) = match diff {
            Ok(GitDiff::Repository { numstat, .. }) => numstat_totals(&numstat),
            Ok(GitDiff::NotRepository) => (0, 0),
            Err(error) => {
                tracing::warn!("working tree: git diff failed: {error}");
                (0, 0)
            }
        };
        Ok(WorkingTree {
            branch,
            added,
            removed,
        })
    }

    /// `/export <文件名>`：经执行环境写文件（tui-manage.md §3）。
    async fn write_export(
        &self,
        file: rust_cli_kernel_api::manage::ExportFile,
    ) -> Result<String, KernelError> {
        export::write(
            self.machine().info().as_ref(),
            self.machine().files().as_ref(),
            &self.operations,
            file,
        )
        .await
    }

    /// `/permissions` 的列表（tui-manage.md §1.1）。
    async fn permission_rules(
        &self,
        query: rust_cli_kernel_api::permission::PermissionRulesQuery,
    ) -> Result<Vec<rust_cli_kernel_api::permission::SourcedRule>, KernelError> {
        let layer = self.session_rules(query.session.as_ref()).await?;
        permissions::listing(&self.gate, self.ports.store.as_ref(), &query, &layer).await
    }

    /// `/permissions` 的修改：落库后换上新快照（tui-manage.md §1.1）；会话层的删除交给打开着的会话（permission-updates.md §3）。
    async fn change_permission_rules(
        &self,
        change: rust_cli_kernel_api::permission::PermissionRulesChange,
    ) -> Result<Vec<rust_cli_kernel_api::permission::SourcedRule>, KernelError> {
        let session = match &change.session {
            Some(id) => self.settled(id).await,
            None => None,
        };
        let target = session.as_ref().map(|session| {
            (
                session.as_ref() as &dyn SessionHandle,
                self.ports.ids.command(),
            )
        });
        let query = rust_cli_kernel_api::permission::PermissionRulesQuery {
            workspace: change.workspace.clone(),
            session: change.session.clone(),
        };
        permissions::change(&self.gate, self.ports.store.as_ref(), target, change).await?;
        self.permission_rules(query).await
    }

    /// TUI 的斜杠补全：ext 给出的可调用 skill（ext.md §2.7）。
    async fn user_commands(
        &self,
        workspace: &rust_cli_kernel_api::env::WorkspaceRef,
        additional: &[rust_cli_kernel_api::env::EnvPath],
    ) -> Vec<rust_cli_kernel_api::user_command::UserCommand> {
        self.ext
            .user_commands()
            .commands(workspace, additional)
            .await
    }
}

impl LocalKernel {
    /// 打开着的会话的会话层（`/permissions` 的列表）；没有会话或会话没打开时为空（冷打开的会话层本来就是空的）。
    async fn session_rules(
        &self,
        session: Option<&rust_cli_kernel_api::ids::SessionId>,
    ) -> Result<Vec<rust_cli_kernel_api::permission::SourcedRule>, KernelError> {
        let Some(session) = session else {
            return Ok(Vec::new());
        };
        match self.settled(session).await {
            Some(open) => Ok(open.view().await?.session_rules),
            None => Ok(Vec::new()),
        }
    }

    /// 建会话之前看调用方给的 id（session-facts.md §6）：库里有、是同一个工作区的顶层交互会话 → None（回原 id，删除了的
    /// 也算：id 不能复用）；库里有、是别的会话 → `Exists`；库里没有、打开着 → 草稿（只有建会话会建草稿）：同一个工作区时
    /// None，否则 `Exists`；都没有 → 占住会话表的条目，由调用方建。
    async fn claim_new(
        &self,
        id: &SessionId,
        workspace: &rust_cli_kernel_api::env::WorkspaceRef,
    ) -> Result<Option<table::Loading<'_>>, KernelError> {
        let claim = self.claim(id).await;
        let exists = || KernelError::Exists {
            session: id.clone(),
        };
        let stored = self
            .ports
            .store
            .session_facts(std::slice::from_ref(id))
            .await?;
        if let Some(facts) = stored.first() {
            let same = facts.kind == SessionKind::Interactive
                && facts.parent.is_none()
                && facts.workspace == *workspace;
            return if same { Ok(None) } else { Err(exists()) };
        }
        match claim {
            table::Claim::Open(draft) => {
                if draft.view().await?.meta.workspace == *workspace {
                    Ok(None)
                } else {
                    Err(exists())
                }
            }
            table::Claim::Load(claimed) => Ok(Some(claimed)),
        }
    }
}
