use super::*;

fn args(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

/// Host 给的参数（`app-server --stdio --surface desktop`）与它的两种写法；不是这个子命令时不认。
#[test]
fn the_host_arguments_select_the_app_server() {
    assert_eq!(
        parse(&args("app-server --stdio --surface desktop")),
        Some(Ok(Parsed {
            surface: Some("desktop".to_owned()),
            ..Parsed::default()
        }))
    );
    assert_eq!(
        parse(&args("app-server --surface=desktop --stdio")),
        Some(Ok(Parsed {
            surface: Some("desktop".to_owned()),
            ..Parsed::default()
        }))
    );
    assert_eq!(
        parse(&args("app-server --stdio")),
        Some(Ok(Parsed::default()))
    );
    assert!(matches!(parse(&args("app-server")), Some(Err(_))));
    assert!(matches!(
        parse(&args("app-server --stdio --surface")),
        Some(Err(_))
    ));
    assert!(matches!(
        parse(&args("app-server --stdio -p")),
        Some(Err(_))
    ));
    assert_eq!(parse(&args("-p hello")), None);
    assert_eq!(parse(&[]), None);
}

/// `--protocol`（protocol-v5.md §2）：缺省 v4（以前的路径逐字不变），v5 进新的前端。
#[test]
fn the_protocol_option_selects_the_front_end() {
    assert_eq!(
        parse(&args("app-server --stdio --protocol v5")),
        Some(Ok(Parsed {
            protocol: Protocol::V5,
            ..Parsed::default()
        }))
    );
    assert_eq!(
        parse(&args("app-server --stdio --protocol=v4 --surface desktop")),
        Some(Ok(Parsed {
            surface: Some("desktop".to_owned()),
            ..Parsed::default()
        }))
    );

    assert_eq!(
        parse(&args("app-server --stdio --protocol v6")),
        Some(Err(
            "error: option '--protocol <protocol>' argument 'v6' is invalid. Allowed choices are v4, v5.".to_owned()
        ))
    );
    assert!(matches!(
        parse(&args("app-server --stdio --protocol")),
        Some(Err(_))
    ));
    // `--expect-build` 不再认（2026-11-05）：同包由握手里的契约指纹判定（03 §3.7），不要启动参数
    assert_eq!(
        parse(&args(
            "app-server --stdio --protocol v5 --expect-build 1.2.3"
        )),
        Some(Err("error: unknown option '--expect-build'".to_owned()))
    );
}

/// `--frontend`（context.md §15.5）：App 的 Host 给的能力名单（两种写法）；不认识的名字报错。
#[test]
fn the_frontend_option_declares_the_traits() {
    use super::super::app::Declared;
    assert_eq!(
        parse(&args(
            "app-server --stdio --protocol v5 --frontend typed-mentions,code-comments,file-links,gen-ui"
        )),
        Some(Ok(Parsed {
            protocol: Protocol::V5,
            frontend: vec![
                Declared::TypedMentions,
                Declared::CodeComments,
                Declared::FileLinks,
                Declared::GenUi,
            ],
            ..Parsed::default()
        }))
    );
    assert_eq!(
        parse(&args("app-server --stdio --frontend=file-links")),
        Some(Ok(Parsed {
            frontend: vec![Declared::FileLinks],
            ..Parsed::default()
        }))
    );
    assert!(matches!(
        parse(&args("app-server --stdio --frontend desktop")),
        Some(Err(_))
    ));
    assert!(matches!(
        parse(&args("app-server --stdio --frontend")),
        Some(Err(_))
    ));
}

#[test]
fn the_database_id_hides_the_path_and_is_stable_in_a_process() {
    let path = std::path::Path::new("/Users/dev/.zcode/cli/db/db.sqlite");
    let id = database_id(path);
    assert_eq!(id, database_id(path));
    assert!(!id.contains("Users"));
    assert!(id.len() <= 128);
}
