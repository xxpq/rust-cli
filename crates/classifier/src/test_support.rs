//! 分类器测试共用的替身：一个按命令投影的 Bash、按剧本回答的回复与调用上下文。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_base::models;
use rust_cli_kernel_api::classifier::{ClassifierInput, RunTranscript, UserRules};
use rust_cli_kernel_api::message::{ContentBlock, StopReason, Usage};
use rust_cli_kernel_api::model::{BlockDelta, ModelError, ModelEvent, ModelSelection};
use rust_cli_kernel_api::permission::auto_mode::AutoModeRules;
use rust_cli_kernel_api::run_tools::RunTools;
use rust_cli_kernel_api::testing::ListTools;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use rust_cli_kernel_api::tool_names;
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// 投影是命令的工具（同真的 Bash）。
pub(crate) struct Shell(pub(crate) &'static str);

#[async_trait]
impl Tool for Shell {
    fn classifier_input(&self, input: &Value) -> ClassifierInput {
        ClassifierInput::field(input, "command")
    }

    fn name(&self) -> &str {
        self.0
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }

    async fn call(
        &self,
        _input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        Ok(rust_cli_kernel_api::tool::text(String::new()))
    }
}

pub(crate) fn bash() -> Arc<dyn Tool> {
    Arc::new(Shell(tool_names::BASH))
}

/// 一段文字的回复，结束原因由调用方给。
pub(crate) fn reply(text: &str, stop: StopReason) -> Vec<Result<ModelEvent, ModelError>> {
    vec![
        Ok(ModelEvent::MessageStart {
            id: "msg_classifier".to_owned(),
            model: models::SONNET_5.to_owned(),
            usage: Usage::default(),
        }),
        Ok(ModelEvent::BlockStart {
            index: 0,
            block: ContentBlock::Text {
                text: String::new(),
            },
        }),
        Ok(ModelEvent::BlockDelta {
            index: 0,
            delta: BlockDelta::Text(text.to_owned()),
        }),
        Ok(ModelEvent::BlockStop { index: 0 }),
        Ok(ModelEvent::MessageDelta {
            stop_reason: Some(stop),
            usage: Usage::default(),
        }),
        Ok(ModelEvent::MessageStop),
    ]
}

/// 调用上下文：这次运行的工具与摘录。
pub(crate) fn context(
    tools: Vec<Arc<dyn Tool>>,
    transcript: Option<RunTranscript>,
) -> ToolUseContext {
    ToolUseContext {
        tools: Arc::new(RunTools::new(Arc::new(ListTools(tools)), &[])),
        transcript: transcript.map(Arc::new),
        ..rust_cli_kernel_api::testing::tool_use_context()
    }
}

pub(crate) fn glm() -> ModelSelection {
    ModelSelection::new("zhipu", models::GLM_5_3)
}

pub(crate) fn anthropic() -> ModelSelection {
    ModelSelection::new("anthropic", models::OPUS_5_5)
}

static NO_AUTO_MODE_RULES: AutoModeRules = AutoModeRules {
    allow: Vec::new(),
    soft_deny: Vec::new(),
    hard_deny: Vec::new(),
    environment: Vec::new(),
};

/// 没有用户规则（缺省设置、没有 deny 规则）：提示与 B6 的相同。
pub(crate) fn no_rules() -> UserRules<'static> {
    UserRules {
        auto_mode: &NO_AUTO_MODE_RULES,
        deny: &[],
    }
}
