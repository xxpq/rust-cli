use rust_cli_kernel_api::manage::{McpServerInfo, McpServerState};

use super::*;

fn info(state: McpServerState, tools: usize) -> McpServerInfo {
    McpServerInfo {
        name: "docs".to_owned(),
        state,
        reason: None,
        tools,
        machine: Default::default(),
        auth: None,
    }
}

#[test]
fn the_sign_in_lines_say_what_happens_and_give_the_whole_link() {
    assert_eq!(
        signing_in(
            "docs",
            "https://auth.example.com/authorize?x=1",
            Browser::Opened,
            Lang::En
        ),
        "Signing in to docs…\nOpened the sign-in page in your browser. Sign in there.\nhttps://auth.example.com/authorize?x=1"
    );
    assert_eq!(
        signing_in("docs", "https://a.example", Browser::Yourself, Lang::En),
        "Signing in to docs…\nOpen this link on any device and sign in there.\nhttps://a.example"
    );
    assert_eq!(
        signing_in("docs", "https://a.example", Browser::Opened, Lang::Zh),
        "正在登录 docs…\n已在浏览器里打开登录页，请在那里登录。\nhttps://a.example"
    );
}

#[test]
fn the_result_says_whether_the_server_connected() {
    assert_eq!(
        signed_in("docs", &info(McpServerState::Connected, 3), Lang::En),
        "Signed in to docs. Connected · 3 tools."
    );
    assert_eq!(
        signed_in("docs", &info(McpServerState::Connected, 1), Lang::En),
        "Signed in to docs. Connected · 1 tool."
    );
    assert_eq!(
        signed_in("docs", &info(McpServerState::NeedsAuth, 0), Lang::En),
        "Signed in to docs, but it isn't connected (needs authentication)."
    );
    assert_eq!(
        sign_in_failed("docs", "OAuth authorization timed out.", Lang::En),
        "Couldn't sign in to docs: OAuth authorization timed out."
    );
    assert_eq!(sign_in_cancelled("docs", Lang::Zh), "已取消登录 docs。");
}
