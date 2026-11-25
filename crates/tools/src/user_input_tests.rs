use std::sync::Mutex;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::testing::{MemoryEnvironment, ScriptedInteractions};
use rust_cli_kernel_api::tool::{PromptContext, ToolError, ToolOutput};

use super::*;
use crate::test_more::Ctx;

/// 记下输入的 Read 替身：内容是 `read:<路径>`；`unchanged.txt` 给出“没有变化”。
#[derive(Default)]
struct FakeRead {
    inputs: Mutex<Vec<Value>>,
}

#[async_trait]
impl Tool for FakeRead {
    fn classifier_input(
        &self,
        _input: &serde_json::Value,
    ) -> rust_cli_kernel_api::classifier::ClassifierInput {
        rust_cli_kernel_api::classifier::ClassifierInput::Skip
    }

    fn name(&self) -> &str {
        "Read"
    }
    fn input_schema(&self) -> Value {
        json!({})
    }
    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }
    async fn call(
        &self,
        input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        self.inputs.lock().unwrap().push(input.clone());
        let path = input["file_path"].as_str().unwrap_or_default().to_owned();
        let text = if path.ends_with("unchanged.txt") {
            crate::texts::FILE_UNCHANGED.to_owned()
        } else {
            format!("read:{path}")
        };
        Ok(ToolOutput {
            content: vec![ToolResultContent::Text { text }],
            is_error: false,
            display: Value::Null,
        })
    }
}

/// 拒绝 `secret.txt` 的判决链替身。
struct DenySecret;

#[async_trait]
impl PermissionGate for DenySecret {
    async fn decide(&self, _: &dyn Tool, _: &Value, _: &ToolUseContext) -> PermissionDecision {
        PermissionDecision::Passthrough
    }
    async fn local_rules_committed(&self, _: &WorkspaceRef) {}
    async fn session_rules_committed(
        &self,
        _: &rust_cli_kernel_api::ids::SessionId,
        _: &WorkspaceRef,
    ) {
    }
    async fn check_rules(
        &self,
        _tool: &dyn Tool,
        input: &Value,
        _context: &ToolUseContext,
    ) -> Option<PermissionDecision> {
        let path = input["file_path"].as_str()?;
        path.ends_with("secret.txt")
            .then(|| PermissionDecision::Deny {
                message: "denied".to_owned(),
            })
    }
}

fn environment() -> MemoryEnvironment {
    MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/Users/dev/work"),
        platform: "darwin".to_owned(),
        os_version: String::new(),
        shell: "zsh".to_owned(),
        is_git_repo: false,
        home: Some(EnvPath::new("/Users/dev")),
        uid: None,
    })
    .with_file("/Users/dev/work/a.txt", "alpha\n")
    .with_file("/Users/dev/work/secret.txt", "s\n")
    .with_file("/Users/dev/work/unchanged.txt", "u\n")
    .with_file("/Users/dev/work/dir/b.txt", "b\n")
    .with_file("/Users/dev/work/dir/sub/c.txt", "c\n")
    .with_file("/Users/dev/work/big.txt", &"x".repeat(300_000))
    .with_file("/Users/dev/notes.md", "n\n")
}

fn tools(read: Arc<FakeRead>) -> UserInputTools {
    UserInputTools {
        read: Some(read),
        bash: crate::bash::BashTool::default(),
        gate: Arc::new(DenySecret),
    }
}

fn context() -> ToolUseContext {
    let mut ctx = Ctx::new(Arc::new(ScriptedInteractions::new([])));
    ctx.set_env(Arc::new(environment()));
    ctx.build()
}

#[tokio::test]
async fn mentions_become_file_directory_and_reference_attachments() {
    let read = Arc::new(FakeRead::default());
    let input =
        "check @a.txt#L2-3, @dir @big.txt @missing.txt @secret.txt @unchanged.txt @~/notes.md";
    let found = tools(read.clone()).mentions(input, context()).await;
    let sources: Vec<&str> = found.iter().map(|a| a.source.as_str()).collect();
    assert_eq!(
        sources,
        ["file", "directory", "at_mention_reference", "file"]
    );
    assert!(found[0].text.contains(
        "{\"file_path\":\"/Users/dev/work/a.txt\"}\nResult of calling the Read tool:\nread:/Users/dev/work/a.txt"
    ));
    assert_eq!(found[0].data["displayPath"], "a.txt");
    assert!(
        found[1]
            .text
            .ends_with("Result of calling the Bash tool:\nb.txt\nsub\n</system-reminder>")
    );
    assert!(found[2].text.contains("/Users/dev/work/big.txt (293KB)"));
    assert!(found[3].text.contains("read:/Users/dev/notes.md"));
    // 行号范围交给 Read；被拒绝的、不存在的、太大的、目录都不调 Read。
    let inputs = read.inputs.lock().unwrap().clone();
    assert_eq!(
        inputs[0],
        json!({"file_path": "/Users/dev/work/a.txt", "offset": 2, "limit": 2})
    );
    assert_eq!(inputs.len(), 3);
}

/// 附上的文件按提及整个文件读（session-lifecycle.md §10.14）：不带行号范围，被拒绝的、不存在的跳过，顺序照给出的。
#[tokio::test]
async fn attached_files_are_read_like_whole_file_mentions() {
    let read = Arc::new(FakeRead::default());
    let file = |path: &str| rust_cli_kernel_api::input_extras::AttachedFile {
        path: path.to_owned(),
        name: "shown".to_owned(),
        media_type: "text/plain".to_owned(),
        size: 1,
    };
    let files = [
        file("/Users/dev/work/a.txt"),
        file("/Users/dev/work/missing.txt"),
        file("/Users/dev/work/secret.txt"),
        file("/Users/dev/notes.md"),
    ];
    let found = tools(read.clone()).attached(&files, context()).await;
    let sources: Vec<&str> = found.iter().map(|a| a.source.as_str()).collect();
    assert_eq!(sources, ["file", "file"]);
    assert!(found[0].text.contains("read:/Users/dev/work/a.txt"));
    assert!(found[1].text.contains("read:/Users/dev/notes.md"));
    let inputs = read.inputs.lock().unwrap().clone();
    assert_eq!(inputs[0], json!({"file_path": "/Users/dev/work/a.txt"}));
    assert_eq!(inputs.len(), 2);
}

#[tokio::test]
async fn without_a_read_tool_nothing_is_attached() {
    let tools = UserInputTools {
        read: None,
        bash: crate::bash::BashTool::default(),
        gate: Arc::new(DenySecret),
    };
    assert!(tools.mentions("@a.txt", context()).await.is_empty());
}

/// ATTACH-2（session-lifecycle.md §10.41）：媒体文件（图片、PDF、视频）不经 `attached` 按提及读，在开回合时经 `media` 读。
#[tokio::test]
async fn attached_leaves_media_files_to_media() {
    let read = Arc::new(FakeRead::default());
    let file = |path: &str, media_type: &str| rust_cli_kernel_api::input_extras::AttachedFile {
        path: path.to_owned(),
        name: "shown".to_owned(),
        media_type: media_type.to_owned(),
        size: 1,
    };
    let files = [
        file("/Users/dev/work/a.txt", "text/plain"),
        file("/Users/dev/work/pic.png", "image/png"),
        file("/Users/dev/work/doc.pdf", "application/pdf"),
        file("/Users/dev/work/clip.mp4", "video/mp4"),
    ];
    let found = tools(read.clone()).attached(&files, context()).await;
    assert_eq!(found.len(), 1);
    assert_eq!(
        read.inputs.lock().unwrap().clone(),
        [json!({"file_path": "/Users/dev/work/a.txt"})]
    );
}

/// 本机的媒体文件先过判决链的 deny 规则（同提及）：被拒绝的不读、不给模型；不是媒体的不在这里读。
#[tokio::test]
async fn media_skips_files_the_rules_deny() {
    let read = Arc::new(FakeRead::default());
    let file = |path: &str, media_type: &str| rust_cli_kernel_api::input_extras::AttachedFile {
        path: path.to_owned(),
        name: "shown".to_owned(),
        media_type: media_type.to_owned(),
        size: 1,
    };
    let files = [
        // 规则替身按路径的结尾拒绝 `secret.txt`
        file("/Users/dev/work/secret.txt", "video/mp4"),
        file("/Users/dev/work/a.txt", "text/plain"),
        file("/Users/dev/work/dir", "video/mp4"),
    ];
    let media = tools(read.clone()).media(&files, &[], context()).await;
    assert!(media.content.is_empty());
    let texts: Vec<&str> = media.attachments.iter().map(|a| a.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "[Attached video/mp4: /Users/dev/work/dir]\n[Media omitted from provider request because the selected model does not support video input.]"
        ]
    );
    assert!(read.inputs.lock().unwrap().is_empty());
}
