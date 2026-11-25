//! 附加工作目录的 owner 是会话 actor（docs/specs/rust-cli/session-dirs.md §3）：起始的目录进视图与每一次 `RunView`；
//! `AddDirectory` 由 actor 判定、追加、发事件并记进对话；回合在跑时加的，下一次工具调用就看到。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::message::{ContentBlock, Message, ToolResultContent, UserOrigin};
use rust_cli_kernel_api::session_api::{Ack, Command, CommandResult, KernelEvent, SessionHandle};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response, tool_use_response};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use rust_cli_kernel_api::user_command::LOCAL_COMMAND_OUTPUT_SOURCE;
use rust_cli_kernel_api::working_dirs::{self, Addition, Covered, Remembered};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};

use crate::test_support::{Setup, envelope, finished, submit, until};

/// 报告自己的工具上下文里的附加目录；`gate` 有值时先等它（让测试在回合中途发命令）。
struct Dirs {
    gate: Option<Arc<Notify>>,
}

#[async_trait]
impl Tool for Dirs {
    fn classifier_input(
        &self,
        _input: &serde_json::Value,
    ) -> rust_cli_kernel_api::classifier::ClassifierInput {
        rust_cli_kernel_api::classifier::ClassifierInput::Skip
    }

    fn name(&self) -> &str {
        "Dirs"
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        if input["wait"] == json!(true)
            && let Some(gate) = &self.gate
        {
            gate.notified().await;
        }
        let text = context
            .additional_directories
            .iter()
            .map(EnvPath::as_str)
            .collect::<Vec<_>>()
            .join(",");
        Ok(ToolOutput {
            content: vec![ToolResultContent::Text { text }],
            is_error: false,
            display: Value::Null,
        })
    }
}

fn results(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .flat_map(|message| match message {
            Message::User(user) => user.content.clone(),
            _ => Vec::new(),
        })
        .filter_map(|block| match block {
            ContentBlock::ToolResult { content, .. } => match content.as_slice() {
                [ToolResultContent::Text { text }] => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// 本地命令记录里的输出（`<local-command-stdout>` 那一条）。
fn recorded_outputs(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user)
                if matches!(&user.origin, UserOrigin::Meta { source } if source == LOCAL_COMMAND_OUTPUT_SOURCE) =>
            {
                match user.content.as_slice() {
                    [ContentBlock::Text { text }] => Some(text.clone()),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}

fn add(id: &str, directory: &str, args: &str) -> rust_cli_kernel_api::session_api::CommandEnvelope {
    envelope(
        id,
        Command::AddDirectory {
            directory: EnvPath::new(directory),
            args: args.to_owned(),
            remember: false,
        },
    )
}

fn addition(ack: Ack) -> Addition {
    match ack {
        Ack::Accepted {
            result: CommandResult::DirectoryAddition(addition),
        } => addition,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn starting_directories_reach_the_view_and_the_tools() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Dirs", &json!({})),
        text_response(&["done"]),
    ]));
    let mut setup = Setup::new(model);
    setup.tools = vec![Arc::new(Dirs { gate: None })];
    setup.additional_directories = vec![EnvPath::new("/other"), EnvPath::new("/third")];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "go")).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    assert_eq!(results(&view.messages), ["/other,/third"]);
    assert_eq!(
        view.additional_directories,
        [EnvPath::new("/other"), EnvPath::new("/third")]
    );
}

#[tokio::test]
async fn an_added_directory_is_announced_and_recorded() {
    let built = Setup::new(Arc::new(ScriptedModel::new(Vec::new())))
        .build()
        .await;
    let mut events = built.session.subscribe(Some(0));
    assert_eq!(
        addition(built.session.send(add("c1", "/extra", "../extra")).await),
        Addition::Added {
            remembered: Remembered::No
        }
    );
    let seen = until(&mut events, |event| {
        matches!(event, KernelEvent::DirectoryAdded { .. })
    })
    .await;
    assert!(seen.iter().any(|event| matches!(
        event,
        KernelEvent::DirectoryAdded { directory } if directory.as_str() == "/extra"
    )));
    let view = built.session.view().await.unwrap();
    assert_eq!(view.additional_directories, [EnvPath::new("/extra")]);
    assert_eq!(
        recorded_outputs(&view.messages),
        [format!(
            "<local-command-stdout>{}</local-command-stdout>",
            working_dirs::added_record("/extra", &Remembered::No)
        )]
    );
}

/// 已在工作区根或已加的目录里：不改、不发事件，记进对话的是「已在工作目录里」的文字。
#[tokio::test]
async fn a_covered_directory_is_not_added_again() {
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(Vec::new())));
    setup.additional_directories = vec![EnvPath::new("/other")];
    let built = setup.build().await;
    assert_eq!(
        addition(built.session.send(add("c1", "/work/sub", "sub")).await),
        Addition::Covered(Covered {
            by: EnvPath::new("/work"),
            exact: false,
            root: true,
        })
    );
    assert_eq!(
        addition(built.session.send(add("c2", "/other", "/other")).await),
        Addition::Covered(Covered {
            by: EnvPath::new("/other"),
            exact: true,
            root: false,
        })
    );
    let view = built.session.view().await.unwrap();
    assert_eq!(view.additional_directories, [EnvPath::new("/other")]);
    let outputs = recorded_outputs(&view.messages);
    assert_eq!(outputs.len(), 2);
    assert!(
        outputs[0].contains("is already accessible within the current working directory"),
        "{outputs:?}"
    );
    assert!(
        outputs[1].contains("is already added as a working directory"),
        "{outputs:?}"
    );
}

/// 回合在跑时加的目录：下一次工具调用就看到（主循环每次调用前读一次 `RunView`）；记录留到回合收尾。
#[tokio::test]
async fn a_directory_added_mid_turn_reaches_the_next_tool_call() {
    let gate = Arc::new(Notify::new());
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Dirs", &json!({"wait": true})),
        tool_use_response("toolu_2", "Dirs", &json!({})),
        text_response(&["done"]),
    ]));
    let mut setup = Setup::new(model);
    setup.tools = vec![Arc::new(Dirs {
        gate: Some(gate.clone()),
    })];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "go")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::ToolStarted { .. })
    })
    .await;
    assert_eq!(
        addition(built.session.send(add("c2", "/extra", "/extra")).await),
        Addition::Added {
            remembered: Remembered::No
        }
    );
    gate.notify_one();
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    assert_eq!(results(&view.messages), ["", "/extra"]);
    // 记录在回合收尾之后（tui-manage.md §11.3）：排在最后一条 assistant 之后。
    let last_record = view
        .messages
        .iter()
        .rposition(|message| {
            matches!(message, Message::User(user) if matches!(&user.origin, UserOrigin::Meta { source } if source == LOCAL_COMMAND_OUTPUT_SOURCE))
        })
        .unwrap();
    let last_assistant = view
        .messages
        .iter()
        .rposition(|message| matches!(message, Message::Assistant(_)))
        .unwrap();
    assert!(last_record > last_assistant);
}

/// 确认框里选了「记住」（permission-updates.md §6.1）：加进会话，另提交 `DirectoriesRemembered`；存不上仍加进会话，文字写原因。
#[tokio::test]
async fn a_remembered_directory_is_saved_or_says_why_not() {
    use rust_cli_kernel_api::store::{Fact, LocalSettings};
    let remember = |id: &str, directory: &str| {
        envelope(
            id,
            Command::AddDirectory {
                directory: EnvPath::new(directory),
                args: directory.to_owned(),
                remember: true,
            },
        )
    };
    let built = Setup::new(Arc::new(ScriptedModel::new(Vec::new())))
        .build()
        .await;
    assert_eq!(
        addition(built.session.send(remember("c1", "/extra")).await),
        Addition::Added {
            remembered: Remembered::Saved
        }
    );
    let view = built.session.view().await.unwrap();
    assert_eq!(view.additional_directories, [EnvPath::new("/extra")]);
    assert!(
        built
            .store
            .facts(&view.meta.id)
            .contains(&Fact::DirectoriesRemembered {
                directories: vec![EnvPath::new("/extra")],
            })
    );
    assert_eq!(
        built
            .store
            .project_rules(&view.meta.workspace)
            .await
            .unwrap()
            .additional_directories,
        [EnvPath::new("/extra")]
    );
    assert_eq!(
        recorded_outputs(&view.messages),
        [format!(
            "<local-command-stdout>{}</local-command-stdout>",
            working_dirs::added_record("/extra", &Remembered::Saved)
        )]
    );

    // 存储写不进去：仍加进会话。
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(Vec::new())));
    setup.stored = false;
    let built = setup.build().await;
    let Addition::Added {
        remembered: Remembered::Failed { .. },
    } = addition(built.session.send(remember("c1", "/extra")).await)
    else {
        panic!("a failed save still adds the directory");
    };
    let view = built.session.view().await.unwrap();
    assert_eq!(view.additional_directories, [EnvPath::new("/extra")]);
}
