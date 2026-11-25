//! shell 文本的单引号写法（docs/specs/rust-cli/mature-libraries.md §1.1 A4）：几个 crate 拼 bash / POSIX sh 的命令，两种写法各
//! 只定义一处（以前各写了两到三份）。纯计算。两种写法的输出不同，各有照着的参照，不能互换。

/// 总是加单引号，`'` 写成 `'\''`：Bash 工具的脚本、hook 命令的参数、ssh 远端再解析一次的命令。
pub fn single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// 只在需要时加引号：空串写成 `''`；只含 `[A-Za-z0-9_./:=@+,-]` 的原样；其余加单引号，`'` 写成 `'"'"'`。
pub fn quote(text: &str) -> String {
    if text.is_empty() {
        return "''".to_owned();
    }
    let plain = text
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"_./:=@+,-".contains(&byte));
    if plain {
        return text.to_owned();
    }
    format!("'{}'", text.replace('\'', r#"'"'"'"#))
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod tests;
