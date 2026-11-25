use super::*;

fn env(vars: &[(&str, &str)]) -> EnvSnapshot {
    vars.iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

/// App 的 Host 给的那一组（context.md §15.5）：不声明 `shell-escape`（App 没有 bash 模式）；渲染能力归 App 前端层。
#[test]
fn the_app_declares_its_renders_and_typed_input() {
    // 本机平台的绝对路径（Windows 上 `/Users/dev` 不是绝对路径）
    let root = if cfg!(windows) {
        r"C:\Users\dev\.zcode\visualizations"
    } else {
        "/Users/dev/.zcode/visualizations"
    };
    let declared = parse("typed-mentions,code-comments, file-links,gen-ui").unwrap();
    let (traits, renders) = declare(&declared, &env(&[(GEN_UI_OUTPUT_ROOT, root)])).unwrap();
    assert!(!traits.shell_escape);
    assert!(traits.typed_mentions);
    assert!(renders.code_comments && renders.file_links);
    assert_eq!(renders.gen_ui, Some(EnvPath::new(root)));
}

/// 名字是闭集：不认识的报错并写出允许的名字。
#[test]
fn an_unknown_name_is_refused() {
    assert_eq!(
        parse("code-comments,desktop"),
        Err("error: option '--frontend <traits>' argument 'desktop' is invalid. Allowed choices are shell-escape, typed-mentions, code-comments, file-links, gen-ui.".to_owned())
    );
}

/// 声明了 `gen-ui` 却没有输出根目录（或不是绝对路径）是 Host 的错：启动失败，不降级。
#[test]
fn gen_ui_needs_an_absolute_root() {
    let declared = parse("gen-ui").unwrap();
    for vars in [vec![], vec![(GEN_UI_OUTPUT_ROOT, "visualizations")]] {
        assert!(declare(&declared, &env(&vars)).is_err(), "{vars:?}");
    }
    // 没声明时不看变量
    let none = declare(&[], &env(&[])).unwrap();
    assert_eq!(none, (FrontendTraits::default(), Renders::default()));
}
