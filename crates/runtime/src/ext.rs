//! skill、MCP 与插件的入口配置（docs/specs/rust-cli/ext.md §1、§6）；清单来源的合并见 listings.rs。
//! 插件的 agent 与 hooks 在这里转成 agents 与 hooks 的类型（ext.md §4）。

use std::sync::Arc;

use rust_cli_agents::PluginAgentFile;
use rust_cli_ext::PluginSet;
use rust_cli_hooks::PluginHooks;
use rust_cli_kernel_api::cron_store::CronStore;
use rust_cli_kernel_api::frontend_prompt::FrontendPrompt;
use rust_cli_kernel_api::mcp::McpAdmission;

pub use rust_cli_ext::ExtConfig;

/// 入口层给内核的扩展配置：配置快照与持久定时任务的存储。MCP 传输在机器上（`MachineEnv::mcp()`，经 `Ports` 的注册表，
/// remote-exec.md §15.8.1）。
#[derive(Clone)]
pub struct Extensions {
    pub config: ExtConfig,
    /// 持久定时任务的存储（入口给本地的 `LocalCronStore`，第二阶段在 App 下换成 App 的实现；tools-more.md
    /// §16.2）。随任务表一起建好（F11，审查 R1-13：原来内核建好后再经 `with_cron_store` 装上）；None 时没有
    /// 持久任务（测试、嵌入）。`-p` 也给：CronCreate `durable: true` 照常写文件，只是不调度。
    pub cron_store: Option<Arc<dyn CronStore>>,
    /// 本机跨会话通信（docs/specs/rust-cli/cross-session.md §2.4）：入口给本地的 `LocalPeers`；缺省关闭。
    pub peers: rust_cli_session::PeerDeps,
    /// `-p` 的输入是 stream-json（不是单次的 print 会话，headless.md §6.1，修复线 F39）：Bash 后台命令的
    /// 缺省期限是 1800000。
    pub streaming_input: bool,
    /// Monitor 开着（工作线 F83，tools-more.md §19.1）：注册 Monitor，Bash 与 CronCreate 的描述随之变，Bash 拦前台的
    /// 长 `sleep`。产品缺省开，由入口给（只给测试用的变量关掉，Windows 上要有 Git Bash）；这里缺省关（测试与嵌入）。
    pub monitor: bool,
    /// 输入账本（pg-store.md §4，kernel-api `input_journal`）：有持久账本的入口给会话库的那一个，其余是 `NoJournal`。
    pub journal: Arc<dyn rust_cli_kernel_api::input_journal::InputJournal>,
    /// 会话的策略（kernel-api `session_policy`）：入口按部署形态算一次，内核各处只读它。缺省是一个进程独占一个本机
    /// 会话库时的行为。
    pub policy: rust_cli_kernel_api::session_policy::SessionPolicy,
    /// 工作区声明的 MCP 服务器的准入（App 形态，docs/specs/rust-cli/ext.md §3.1 与 18-mcp-trust.md §12.4）：入口按工作区给；None 时项目级
    /// 服务器照常连（TUI、`-p`、测试）。
    pub mcp_admission: Option<Arc<dyn McpAdmission>>,
    /// 进程级的授权登记（OAuth，docs/specs/rust-cli/ext.md §3.12.6、§3.12.14）：入口为一个进程建一份（包着 platform 的授权端口），
    /// 每套内核都给它；None 时不能 Sign in（v4 的 App、serve、测试）。
    pub mcp_sign_ins: Option<Arc<rust_cli_ext::McpSignIns>>,
    /// 前端层（docs/specs/rust-cli/context.md §15.5）：前端上下文段、`@` 插件的引用、引用与页面状态的写法。入口为每套内核装一份
    /// （按它的插件集）；None 时（测试、嵌入）这些内容不发。
    pub frontend_prompt: Option<Arc<dyn FrontendPrompt>>,
}

impl Extensions {
    /// 没有 MCP、没有插件；skill 照常从执行环境发现。
    pub fn none() -> Self {
        Self {
            config: ExtConfig::default(),
            cron_store: None,
            peers: rust_cli_session::PeerDeps::off(),
            streaming_input: false,
            monitor: false,
            journal: Arc::new(rust_cli_kernel_api::input_journal::NoJournal),
            policy: rust_cli_kernel_api::session_policy::SessionPolicy::default(),
            mcp_admission: None,
            mcp_sign_ins: None,
            frontend_prompt: None,
        }
    }
}

/// 插件里的 agent 文件与 hooks（ext.md §4）；hooks 里的坏项跳过并记警告（hooks.md D1）。内核构造时从这套内核的
/// `PluginSet` 一次算出（ext.md §4.0），之后不变。
pub(crate) fn plugin_parts(plugins: &PluginSet) -> (Vec<PluginAgentFile>, Vec<PluginHooks>) {
    let agents = plugins
        .plugins()
        .iter()
        .flat_map(|plugin| {
            plugin.agent_files.iter().map(|file| PluginAgentFile {
                plugin: plugin.context.clone(),
                path: file.path.clone(),
                namespace: file.namespace.clone(),
            })
        })
        .collect();
    let hooks = plugins
        .plugins()
        .iter()
        .filter(|plugin| !plugin.hooks.is_empty())
        .map(|plugin| {
            let (hooks, warnings) = PluginHooks::from_json(plugin.context.clone(), &plugin.hooks);
            for warning in warnings {
                tracing::warn!(warning, "plugin hooks");
            }
            hooks
        })
        .collect();
    (agents, hooks)
}

#[cfg(test)]
#[path = "ext_tests.rs"]
mod tests;
