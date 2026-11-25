//! 前端上下文段（context.md §15.6）：段落按这套内核能宣布的节选、文字是 Node 原文、增量与「不再适用」、Gen UI 目录与 Node 同一个
//! 算法。从 context crate 搬来（APP-CTX），期望值不变；「插件关时整节省略」现在在建端口时定（mod_tests.rs）。

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};

use super::*;

const ROOT: &str = "/Users/dev/.zcode/visualizations";
/// Node `getGenUiOutputDirectory(ROOT, {workspacePath: "/Users/dev/work", sessionId: "sess_1"})` 的结果（tsx 跑的
/// `packages/shared/src/node/genUiPaths.ts`）。
const NODE_DIRECTORY: &str = "/Users/dev/.zcode/visualizations/098806cf466f5494f9043e655a266b92f265b53e89520857d0ae429bc41f7bca";

fn workspace(root: &str) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root),
    }
}

/// App 声明的那一组（`code-comments,file-links,gen-ui`），visualize 加载了。
fn app() -> Renders {
    Renders {
        code_comments: true,
        file_links: true,
        gen_ui: Some(EnvPath::new(ROOT)),
    }
}

fn scope() -> (WorkspaceRef, SessionId) {
    (workspace("/Users/dev/work"), SessionId::new("sess_1"))
}

/// 全部段落：标题、Gen UI（末尾是这个会话的目录）、文件与链接、评审卡，与 Node 的 Desktop 段逐字相同（目录那一行除外）。
#[test]
fn the_app_gets_the_node_desktop_text() {
    let (workspace, session) = scope();
    let (text, data) = announce(&app(), (&workspace, &session), None).unwrap();
    let node = [
        texts::frontend_heading(),
        texts::frontend_gen_ui(),
        texts::frontend_files(),
        texts::frontend_code_comments(),
    ]
    .join("\n\n");
    let directory_line = format!("- Visualization directory for this session: {NODE_DIRECTORY}");
    assert_eq!(
        text,
        node.replace(
            "privateContent is not sent to the model.\n\n",
            &format!("privateContent is not sent to the model.\n{directory_line}\n\n"),
        )
    );
    assert!(text.starts_with("# ZCode Desktop Context\n\n### Interactive views\n"));
    assert!(text.contains("::code-comment{"));
    assert_eq!(
        data,
        json!({"gen_ui": NODE_DIRECTORY, "file_links": true, "code_comments": true})
    );
}

/// 没有 Gen UI（没声明，或这套内核没加载要它的插件）时整节省略；别的节各按各的能力。
#[test]
fn each_section_follows_its_render() {
    let (workspace, session) = scope();
    let without = Renders {
        gen_ui: None,
        ..app()
    };
    let (text, data) = announce(&without, (&workspace, &session), None).unwrap();
    assert!(!text.contains("Interactive views"), "{text}");
    assert!(text.starts_with("# ZCode Desktop Context\n\n### Files & URLs\n"));
    assert_eq!(data, json!({"file_links": true, "code_comments": true}));
    let comments_only = Renders {
        code_comments: true,
        ..Renders::default()
    };
    let (text, _) = announce(&comments_only, (&workspace, &session), None).unwrap();
    assert!(text.starts_with("# ZCode Desktop Context\n\n### Inline Code Comments\n"));
}

/// 增量：同样的段落不再发；插件晚加载时加上 Gen UI 一节、发整段；前端不再声明时发「不再适用」那一句，之后不再发。
/// 从没宣布过、也什么都没声明（TUI、`-p`）时不发。形状不对的 `data` 当作没宣布过。
#[test]
fn it_is_announced_again_only_when_it_changes() {
    let (workspace, session) = scope();
    let scope = (&workspace, &session);
    let none = Renders::default();
    assert_eq!(announce(&none, scope, None), None);
    let without = Renders {
        gen_ui: None,
        ..app()
    };
    let first = announce(&without, scope, None).unwrap();
    assert_eq!(announce(&without, scope, Some(&first.1)), None);
    let with_gen_ui = announce(&app(), scope, Some(&first.1)).unwrap();
    assert!(with_gen_ui.0.contains("### Interactive views"));
    let gone = announce(&none, scope, Some(&first.1)).unwrap();
    assert_eq!(gone.0, texts::frontend_gone());
    assert_eq!(gone.1, json!({}));
    assert_eq!(announce(&none, scope, Some(&gone.1)), None);
    assert_eq!(announce(&none, scope, Some(&json!("not an object"))), None);
}

/// 目录照 Node：键是 `JSON.stringify([工作区, 会话 id])`（引号、非 ASCII 同 JS 的写法），转义后写进文字（同环境段）。
#[test]
fn the_directory_follows_node() {
    let session = SessionId::new("sess_1");
    assert_eq!(
        gen_ui_directory(ROOT, &workspace("/Users/dev/work"), &session),
        NODE_DIRECTORY
    );
    // Node 的同一个函数：工作区路径带非 ASCII 与引号
    assert_eq!(
        gen_ui_directory(ROOT, &workspace("/Users/dev/wörk \"q\""), &session),
        "/Users/dev/.zcode/visualizations/7408e922ff66f824103c28ca294612c775df5562c8a83356c456b5f3bf4d9fd7"
    );
    let renders = Renders {
        gen_ui: Some(EnvPath::new("/Users/dev/<x>")),
        ..Renders::default()
    };
    let work = workspace("/Users/dev/work");
    let (text, _) = announce(&renders, (&work, &session), None).unwrap();
    assert!(text.contains("session: /Users/dev/&lt;x&gt;/"), "{text}");
}
