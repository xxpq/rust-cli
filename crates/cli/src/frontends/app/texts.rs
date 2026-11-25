//! App 前端层的文字（docs/specs/rust-cli/context.md §15.5）：`packages/rust-cli/prompts/app/` 下的数据文件，编译时嵌入。都是 Node 的
//! 原文（出处在 §15.6、§15.8、§15.11）；CTX-P05 时在 `prompts/context/`，APP-CTX 原样搬来。模板变量写成 `{{name}}`，用 prompts 的
//! `texts::render` 填。

macro_rules! texts {
    ($($name:ident => $file:literal,)*) => {
        $(
            #[doc = concat!("`prompts/app/", $file, ".txt`")]
            pub(super) fn $name() -> &'static str {
                let raw = rust_cli_base::include_lf!(concat!("../../../../../prompts/app/", $file, ".txt"));
                // 数据文件末尾有一个换行（编辑器友好），加载时去掉（同 prompts 的文字）
                raw.strip_suffix('\n').unwrap_or(raw)
            }
        )*

        #[cfg(test)]
        pub(super) const ALL: &[(&str, fn() -> &'static str)] = &[$(($file, $name)),*];
    };
}

texts! {
    // 前端上下文段（§15.6）：Node `core/src/context/sections/desktop.ts`。
    frontend_heading => "frontend_heading",
    frontend_gen_ui => "frontend_gen_ui",
    frontend_gen_ui_directory => "frontend_gen_ui_directory",
    frontend_files => "frontend_files",
    frontend_code_comments => "frontend_code_comments",
    frontend_gone => "frontend_gone",
    // `@` 插件的引用（§15.11）：Node `core/src/plugin-reference/reminder.ts` 的 `renderReminderBody`。
    plugin_reference => "plugin_reference",
    plugin_reference_plugin => "plugin_reference_plugin",
    // 一条输入带的引用与页面状态（§15.8）。
    input_quote => "input_quote",
    input_page_state => "input_page_state",
}
