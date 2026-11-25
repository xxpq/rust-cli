//! 真实系统凭据库的手动核对（docs/specs/rust-cli/platform.md §13.10），由 `scripts/rust-cli/keyring-macos-check.mjs` 在
//! 临时 HOME 的临时钥匙串里调用；不进自动测试。
//!
//! - `read <服务名> <账号名>`：打印 `{"value": …}`（没有时 null）或 `{"error": …}`；
//! - `write <服务名> <账号名> <值>`：打印 `{"ok": true}` 或 `{"error": …}`。
//!
//! macOS 上先关掉钥匙串的用户交互：要弹框的访问（读别的程序建的条目）报错，不在用户的桌面上弹框。

#[tokio::main(flavor = "current_thread")]
#[expect(
    clippy::disallowed_methods,
    reason = "核对用的辅助程序（不是产品代码）读自己的命令行参数"
)]
async fn main() {
    // 关不掉就不碰钥匙串（宁可失败，也不在用户的桌面上弹框）
    #[cfg(target_os = "macos")]
    let Ok(_quiet) =
        security_framework::os::macos::keychain::SecKeychain::disable_user_interaction()
    else {
        print(&serde_json::json!({ "error": "cannot disable keychain user interaction" }));
        return;
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = match args.as_slice() {
        ["read", service, account] => match rust_cli_keyring::read(service, account).await {
            Ok(value) => serde_json::json!({ "value": value }),
            Err(error) => serde_json::json!({ "error": error.to_string() }),
        },
        ["write", service, account, value] => {
            match rust_cli_keyring::write(service, account, value).await {
                Ok(()) => serde_json::json!({ "ok": true }),
                Err(error) => serde_json::json!({ "error": error.to_string() }),
            }
        }
        _ => {
            serde_json::json!({ "error": "usage: keyring_probe read|write <service> <account> [<value>]" })
        }
    };
    print(&output);
}

#[expect(clippy::print_stdout, reason = "辅助程序把结果写到标准输出")]
fn print(value: &serde_json::Value) {
    println!("{value}");
}
