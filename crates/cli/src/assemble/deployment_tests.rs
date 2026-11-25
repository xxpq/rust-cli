//! 部署配置的解析与校验（docs/specs/rust-cli/deployment.md §6）。
#![allow(clippy::disallowed_methods, reason = "测试在临时目录里准备配置文件")]

use super::{
    Deployment, PgStore, PrimaryEnv, PrimarySource, RelayPrimary, Resolved, SessionStoreConfig,
    SshPrimary, Traits, VAR, load,
};
#[cfg(feature = "cloud")]
use super::{PgPool, RelaySource, StoreSource};

#[test]
fn an_empty_config_is_the_local_default() {
    let deployment = Deployment::parse("{}").unwrap();
    assert_eq!(deployment, Deployment::default());
    assert!(deployment.primary_is_local());
}

#[test]
fn an_ssh_primary_env_parses() {
    let text = r#"{
        "primary_env": {"ssh": {"destination": "dev@sandbox", "workdir": "/srv/proj", "ssh_config": "/tmp/ssh.conf"}}
    }"#;
    let deployment = Deployment::parse(text).unwrap();
    assert_eq!(
        deployment.primary_env,
        PrimaryEnv::Ssh(SshPrimary {
            destination: "dev@sandbox".to_owned(),
            workdir: "/srv/proj".to_owned(),
            ssh_config: Some("/tmp/ssh.conf".to_owned()),
            artifact: None,
        })
    );
    assert!(!deployment.primary_is_local());
    assert_eq!(
        Deployment::parse(r#"{"primary_env": "local"}"#).unwrap(),
        Deployment::default()
    );
}

#[test]
fn a_relay_primary_env_parses() {
    let text = r#"{
        "primary_env": {"relay": {
            "url": "wss://relay.example.com/api/exec/v1/chats/chat-a/connect",
            "token_env": "ZCODE_EXEC_TOKEN",
            "user": "u-1",
            "workdir": "/home/z/my-project",
            "up_url": "https://relay.example.com/api/workspaces/chat-a/up"
        }}
    }"#;
    let deployment = Deployment::parse(text).unwrap();
    assert_eq!(
        deployment.primary_env,
        PrimaryEnv::Relay(RelayPrimary {
            url: "wss://relay.example.com/api/exec/v1/chats/chat-a/connect".to_owned(),
            token_env: "ZCODE_EXEC_TOKEN".to_owned(),
            user: "u-1".to_owned(),
            workdir: "/home/z/my-project".to_owned(),
            up_url: Some("https://relay.example.com/api/workspaces/chat-a/up".to_owned()),
        })
    );
    assert!(!deployment.primary_is_local());
    let without_up = r#"{"primary_env": {"relay": {"url": "ws://r.example.com/c", "token_env": "T", "user": "u", "workdir": "/w"}}}"#;
    assert!(Deployment::parse(without_up).is_ok());
}

/// 旧名 `remote` 仍可用（北京 staging 的云端运行壳的配置按它写，remote-exec.md §29.8），解析结果与 `relay` 相同；报错时写
/// 的是新名。
#[test]
fn the_old_name_remote_still_parses_as_relay() {
    let body = r#"{"url": "ws://r.example.com/c", "token_env": "T", "user": "u", "workdir": "/w"}"#;
    let relay = Deployment::parse(&format!(r#"{{"primary_env": {{"relay": {body}}}}}"#)).unwrap();
    let remote = Deployment::parse(&format!(r#"{{"primary_env": {{"remote": {body}}}}}"#)).unwrap();
    assert_eq!(relay, remote);
    assert!(matches!(remote.primary_env, PrimaryEnv::Relay(_)));
    let error = Deployment::parse(
        r#"{"primary_env": {"remote": {"url": "ws://r.example.com/c", "token_env": "T", "user": " ", "workdir": "/w"}}}"#,
    )
    .unwrap_err();
    assert!(error.contains("primary_env.relay.user"), "{error}");
}

/// 每条校验各有一句话指出哪个字段不对。地址的 scheme 要在这里拦住：拨号时才发现的话，exec-client 会一直重连。
#[test]
fn a_bad_relay_primary_env_names_the_field() {
    for (text, field) in [
        (
            r#"{"primary_env": {"relay": {"url": "https://r.example.com/c", "token_env": "T", "user": "u", "workdir": "/w"}}}"#,
            "url",
        ),
        (
            r#"{"primary_env": {"relay": {"url": "ws://r.example.com/c", "token_env": " ", "user": "u", "workdir": "/w"}}}"#,
            "token_env",
        ),
        (
            r#"{"primary_env": {"relay": {"url": "ws://r.example.com/c", "token_env": "T", "user": "", "workdir": "/w"}}}"#,
            "user",
        ),
        (
            r#"{"primary_env": {"relay": {"url": "ws://r.example.com/c", "token_env": "T", "user": "u", "workdir": "proj"}}}"#,
            "workdir",
        ),
        (
            r#"{"primary_env": {"relay": {"url": "ws://r.example.com/c", "token_env": "T", "user": "u", "workdir": "/w", "token": "x"}}}"#,
            "token",
        ),
    ] {
        let error = Deployment::parse(text).unwrap_err();
        assert!(error.contains(field), "{text}\n-> {error}");
    }
}

#[test]
fn a_shared_session_store_parses() {
    let deployment =
        Deployment::parse(r#"{"store": {"pg": {"url_env": "ZCODE_PG_URL", "user_id": "alice"}}}"#)
            .unwrap();
    assert!(deployment.primary_is_local());
    assert_eq!(
        deployment.store,
        SessionStoreConfig::Pg(PgStore {
            url_env: "ZCODE_PG_URL".to_owned(),
            user_id: "alice".to_owned(),
        })
    );
    assert_eq!(
        Deployment::parse(r#"{"store": "local"}"#).unwrap(),
        Deployment::default()
    );
}

#[test]
fn unknown_keys_and_bad_values_are_errors() {
    // 拼错的键不能被悄悄忽略：那样会跑在缺省（本机）上，用户以为在沙盒里；写了别的库不能悄悄落到 SQLite 上（deployment.md §6）。
    for text in [
        r#"{"primary": "local"}"#,
        r#"{"primary_env": {"ssh": {"destination": "h", "workdir": "/w", "port": 22}}}"#,
        r#"{"primary_env": "cloud"}"#,
        r#"{"store": {"postgres": {"url_env": "ZCODE_PG_URL"}}}"#,
        r#"{"store": {"pg": {"url": "postgres://u@h/db", "user_id": "alice"}}}"#,
        r#"{"store": {"pg": {"url_env": "U"}}}"#,
        r#"{"store": {"pg": {"url_env": " ", "user_id": "alice"}}}"#,
        r#"{"store": {"pg": {"url_env": "U", "user_id": " "}}}"#,
        r#"{"store": {"pg": {"url_env": "U", "user_id": "alice", "tenant": "t"}}}"#,
        r#"{"store": {"pg": {"url_env": "U", "user_id": "alice", "role": "owner"}}}"#,
        r#"{"store": {"pg": {"url_env": "U", "user_id": "alice", "user": "bob"}}}"#,
    ] {
        assert!(Deployment::parse(text).is_err(), "{text}");
    }
    let relative = r#"{"primary_env": {"ssh": {"destination": "h", "workdir": "proj"}}}"#;
    let error = Deployment::parse(relative).unwrap_err();
    assert!(error.contains("absolute"), "{error}");
    let empty = r#"{"primary_env": {"ssh": {"destination": " ", "workdir": "/w"}}}"#;
    assert!(
        Deployment::parse(empty)
            .unwrap_err()
            .contains("destination")
    );
    let windows = r#"{"primary_env": {"ssh": {"destination": "h", "workdir": "C:\\work"}}}"#;
    assert!(Deployment::parse(windows).is_ok());
}

#[tokio::test]
async fn the_variable_names_the_file_and_errors_name_the_variable() {
    assert_eq!(
        load(&rust_cli_base::secret::EnvSnapshot::default())
            .await
            .unwrap(),
        Deployment::default()
    );
    let blank = rust_cli_base::secret::EnvSnapshot::from(vec![(VAR.to_owned(), " ".to_owned())]);
    assert_eq!(load(&blank).await.unwrap(), Deployment::default());

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("deployment.json");
    std::fs::write(
        &file,
        br#"{"primary_env": {"ssh": {"destination": "h", "workdir": "/w"}}}"#,
    )
    .unwrap();
    let env = rust_cli_base::secret::EnvSnapshot::from(vec![(
        VAR.to_owned(),
        file.to_string_lossy().into_owned(),
    )]);
    assert!(!load(&env).await.unwrap().primary_is_local());

    let missing = rust_cli_base::secret::EnvSnapshot::from(vec![(
        VAR.to_owned(),
        dir.path().join("nope.json").to_string_lossy().into_owned(),
    )]);
    let error = load(&missing).await.unwrap_err();
    assert!(
        error.to_string().starts_with("ZCODE_DEPLOYMENT="),
        "{error}"
    );
    assert!(error.to_string().contains("no such file"), "{error}");
}

fn snapshot(vars: &[(&str, &str)]) -> rust_cli_base::secret::EnvSnapshot {
    vars.iter().fold(
        rust_cli_base::secret::EnvSnapshot::default(),
        |env, (name, value)| env.with_var(name, value),
    )
}

/// 解析后的部署：`*_env` 指的密钥在这里读成值；主 env 与会话库各自推出后面各步要看的开关。
#[test]
fn a_config_resolves_its_secrets_and_derives_the_switches() {
    let local = Resolved::from_config(&Deployment::default(), &snapshot(&[])).unwrap();
    assert!(local.primary_is_local() && local.store_is_local());
    assert_eq!(
        local.traits(),
        Traits {
            project_config_local: true,
            durable_cron: true,
            memory_on_controller: true,
            task_tools: true,
        }
    );
    let config = Deployment::parse(
        r#"{"primary_env": {"relay": {"url": "wss://relay.example/c/1", "token_env": "T", "user": "u1", "workdir": "/work"}}}"#,
    )
    .unwrap();
    let resolved = Resolved::from_config(&config, &snapshot(&[("T", "ticket")])).unwrap();
    let PrimarySource::Relay(relay) = &resolved.primary else {
        panic!("expected the relay primary");
    };
    assert_eq!(relay.token.expose(), "ticket");
    assert_eq!(
        (relay.user.as_str(), relay.workdir.as_str()),
        ("u1", "/work")
    );
    // 主 env 在别处：跟着主 env 走的三个开关关掉；会话库还是本机的，任务工具照常。
    assert_eq!(
        resolved.traits(),
        Traits {
            project_config_local: false,
            durable_cron: false,
            memory_on_controller: false,
            task_tools: true,
        }
    );
}

/// 共用的会话库（`cloud` 特性）：连接串读成值，任务工具关掉。
#[cfg(feature = "cloud")]
#[test]
fn a_shared_store_resolves_its_url_and_turns_task_tools_off() {
    let config =
        Deployment::parse(r#"{"store": {"pg": {"url_env": "PG", "user_id": "alice"}}}"#).unwrap();
    let env = snapshot(&[("PG", "postgres://db.example/x")]);
    let resolved = Resolved::from_config(&config, &env).unwrap();
    assert!(resolved.primary_is_local() && !resolved.store_is_local());
    let StoreSource::Pg {
        user_id,
        pool: PgPool::Own(url),
    } = &resolved.store
    else {
        panic!("expected a pool this process opens");
    };
    assert_eq!(
        (user_id.as_str(), url.expose()),
        ("alice", "postgres://db.example/x")
    );
    assert!(!resolved.traits().task_tools);
    // 变量没设：解析时就报错，不等到连库。
    let error = Resolved::from_config(&config, &snapshot(&[]))
        .err()
        .unwrap();
    assert_eq!(error.0, "Error: shared session database: PG is not set");
}

/// 缺省构建没有共用的会话库：配置写了它就报错，不回退本机库。
#[cfg(not(feature = "cloud"))]
#[test]
fn a_shared_store_is_an_error_without_the_cloud_feature() {
    let config =
        Deployment::parse(r#"{"store": {"pg": {"url_env": "PG", "user_id": "alice"}}}"#).unwrap();
    let env = snapshot(&[("PG", "postgres://db.example/x")]);
    let error = Resolved::from_config(&config, &env).err().unwrap();
    assert_eq!(error.0, super::NO_SHARED_STORE);
}

/// 中转的服务票的变量没设：解析时就报错（不等到拨号），文字与以前相同。
#[test]
fn an_unset_token_variable_fails_when_resolving() {
    let relay = Deployment::parse(
        r#"{"primary_env": {"relay": {"url": "wss://relay.example/c/1", "token_env": "T", "user": "u1", "workdir": "/work"}}}"#,
    )
    .unwrap();
    let error = Resolved::from_config(&relay, &snapshot(&[])).err().unwrap();
    assert_eq!(
        error.0,
        "Error: primary_env.relay.token_env names T, which is not set"
    );
}

/// serve 握手给的中转与配置文件过同一组规则。
#[cfg(feature = "cloud")]
#[test]
fn a_relay_source_is_checked_like_the_config() {
    let source = |url: &str, user: &str, workdir: &str| RelaySource {
        url: url.to_owned(),
        token: rust_cli_base::secret::RedactedString::new("t".to_owned()),
        user: user.to_owned(),
        workdir: workdir.to_owned(),
        up_url: None,
    };
    assert!(
        source("wss://relay.example/c/1", "u", "/w")
            .validate()
            .is_ok()
    );
    assert!(
        source("https://relay.example", "u", "/w")
            .validate()
            .unwrap_err()
            .contains("ws://")
    );
    assert!(
        source("ws://relay.example", " ", "/w")
            .validate()
            .unwrap_err()
            .contains("user is empty")
    );
    assert!(
        source("ws://relay.example", "u", "w")
            .validate()
            .unwrap_err()
            .contains("absolute")
    );
}
