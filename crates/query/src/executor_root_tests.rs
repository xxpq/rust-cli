//! 工具调用的上下文带这次运行所属的顶层会话（docs/specs/rust-cli/platform.md §5.3）：子代理里的工具发的旁路请求（WebSearch、WebFetch、
//! prompt hook）按它经宿主取账号凭据。顶层会话自己的运行为 None（会话自己）。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::ids::{AgentId, SessionId};
use rust_cli_kernel_api::orchestration::{ToolExecution, ToolExecutor};
use rust_cli_kernel_api::testing::{DirectExecutor, FakeTool, ScriptedModel, text_response};
use rust_cli_kernel_api::tool::{Tool, ToolProgress, ToolUseContext};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::tests::tool_uses;
use crate::run::tests::{parts, request, scope};

/// 记下每次调用的上下文里的顶层会话，再照常执行。
#[derive(Default)]
struct Roots(Mutex<Vec<Option<SessionId>>>);

#[async_trait]
impl ToolExecutor for Roots {
    async fn execute(
        &self,
        tool: Arc<dyn Tool>,
        input: Value,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> ToolExecution {
        self.0.lock().unwrap().push(context.root.clone());
        DirectExecutor.execute(tool, input, context, progress).await
    }
}

/// 跑一次调用 Read 的运行（`root` 是请求的顶层会话），交出执行管线收到的顶层会话。
async fn roots_seen(root: Option<SessionId>) -> Vec<Option<SessionId>> {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_uses(&[("t1", "Read", json!({}))]),
        text_response(&["ok"]),
    ]));
    let roots = Arc::new(Roots::default());
    let mut parts = parts(model, vec![Arc::new(FakeTool::new("Read", true, "text"))]);
    parts.executor = roots.clone();
    let mut request = request("go");
    if root.is_some() {
        request.agent = Some(AgentId::new("agent_1"));
    }
    request.root = root;
    let (scope, _sink) = scope(parts, &request, Abort::new());
    crate::run::run(scope, request).await.unwrap();
    roots.0.lock().unwrap().clone()
}

#[tokio::test]
async fn a_tool_call_in_a_subagent_names_the_root_session() {
    assert_eq!(
        roots_seen(Some(SessionId::new("sess_root"))).await,
        [Some(SessionId::new("sess_root"))]
    );
    // 顶层会话自己的运行：None 是会话自己（同 `QueryRequest::root` 的约定）
    assert_eq!(roots_seen(None).await, [None]);
}
