//! 入口层组装的配置快照交给内核（docs/specs/rust-cli/config.md §3）：整份替换，内核不读文件。

use rust_cli_hooks::HooksConfig;
use rust_cli_kernel_api::permission::PermissionRules;
use rust_cli_kernel_api::session_prompt::SessionPrompt;
use rust_cli_session::TitleModel;

use crate::LocalKernel;

impl LocalKernel {
    /// 打开会话标题的生成（TUI；`-p` 不生成，Node 相同，docs/specs/rust-cli/titles.md §2）。在建会话之前
    /// 调用：之后开的会话 actor 才带上它。
    #[must_use]
    pub fn with_titles(mut self, titles: TitleModel) -> Self {
        self.titles = Some(titles);
        self
    }

    /// 新会话钉的模型可见输入（docs/specs/rust-cli/context.md §15.3、§15.5）：入口按命令行与前端能力组装。在建会话之前调用。
    #[must_use]
    pub fn with_prompt(mut self, prompt: SessionPrompt) -> Self {
        self.prompt = std::sync::Arc::new(prompt);
        self
    }

    /// 换上入口层从配置文件组装的 hooks 快照（hooks.md §2.2）；正在运行的事件用旧快照。
    pub fn replace_hooks(&self, config: HooksConfig) {
        self.hooks.replace(config);
    }

    /// 换上入口层从命令行与配置组装的权限规则快照（shell-permissions.md §5）；之后的判决用新快照。
    pub fn replace_permission_rules(&self, rules: PermissionRules) {
        self.gate.replace_rules(rules);
    }
}
