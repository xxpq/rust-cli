//! `zcode plugin` 的集成测试（plugin-store.md §16）：临时 HOME，本地目录市场与本地 zip 市场（本机 HTTP 服务给 zip，不联网）。
#![allow(
    clippy::disallowed_methods,
    reason = "测试在临时目录里准备市场、读回存储，本机 HTTP 服务直接用 tokio"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rust_cli_base::clock::SystemClock;
use rust_cli_env_local::LocalEnvironment;
use rust_cli_net::{Egress, NetworkPolicy};
use rust_cli_paths::StoragePaths;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::plugin_admin::Scope;

struct Fixture {
    dir: tempfile::TempDir,
    admin: PluginAdmin,
    cwd: String,
}

fn write(path: &Path, text: impl AsRef<[u8]>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

impl Fixture {
    async fn new(env: &[(&str, String)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let work = dir.path().join("work");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let paths = StoragePaths::resolve(&Default::default(), &home, &home);
        let process_env = env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        let admin = PluginAdmin::new(
            (Arc::new(LocalEnvironment::new(Some(home.clone()))), home),
            paths,
            process_env,
            Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap()),
            (
                Arc::new(SystemClock::utc()),
                crate::frontends::app::AppFrontend::Absent.unhosted(),
            ),
        );
        Self {
            cwd: work.display().to_string(),
            dir,
            admin,
        }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.dir.path().join(relative)
    }

    fn store(&self, relative: &str) -> PathBuf {
        self.path("home/.zcode/cli/plugins").join(relative)
    }

    async fn run(&self, args: &[&str]) -> Output {
        self.run_with(args, false).await
    }

    async fn run_with(&self, args: &[&str], interactive: bool) -> Output {
        let mut full = vec!["plugin".to_owned()];
        full.extend(args.iter().map(|arg| (*arg).to_owned()));
        let command = match parse(&full).unwrap() {
            Ok(command) => command,
            Err(text) => {
                return Output {
                    code: 1,
                    stdout: String::new(),
                    stderr: text,
                };
            }
        };
        let mut context = Context {
            admin: &self.admin,
            cwd: &self.cwd,
            interactive,
            confirm: Box::new(|_| Box::pin(async { false })),
            cancel: CancellationToken::new(),
        };
        run(command, &mut context).await
    }

    /// 本地目录市场 `mkt`：`kit`（依赖 `base`）与 `base`。
    fn market(&self, kit_version: &str) {
        write(
            &self.path("mkt/.claude-plugin/marketplace.json"),
            json!({"name": "mkt", "owner": {"name": "Dev"}, "plugins": [
                {"name": "kit", "source": "./kit", "description": "Kit", "dependencies": ["base"]},
                {"name": "base", "source": "./base"}
            ]})
            .to_string(),
        );
        for (name, version) in [("kit", kit_version), ("base", "0.1.0")] {
            write(
                &self.path(&format!("mkt/{name}/.claude-plugin/plugin.json")),
                json!({"name": name, "version": version, "description": format!("{name} plugin")})
                    .to_string(),
            );
            write(
                &self.path(&format!("mkt/{name}/skills/{name}/SKILL.md")),
                "---\ndescription: hi\n---\nHi\n",
            );
        }
    }
}

fn json_out(output: &Output) -> Value {
    assert_eq!(output.code, 0, "{output:?}");
    serde_json::from_str(&output.stdout).unwrap()
}

#[tokio::test]
async fn a_directory_marketplace_goes_through_the_whole_lifecycle() {
    let f = Fixture::new(&[]).await;
    f.market("1.0.0");
    let mkt = f.path("mkt").display().to_string();
    let added = f.run(&["marketplace", "add", &mkt, "--json"]).await;
    assert_eq!(json_out(&added)["id"], "mkt");
    let listed = json_out(&f.run(&["marketplace", "list", "--json"]).await);
    let ids: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["zcode-plugins-official", "claude-plugins-official", "mkt"]
    );

    // 裸名字在可装的条目里唯一：装上，依赖一起装，缺省启用。
    let installed = f.run(&["install", "kit"]).await;
    assert_eq!(
        installed.stdout,
        "Installed plugin base@mkt (0.1.0) [enabled]\nInstalled plugin kit@mkt (1.0.0) [enabled]\n"
    );
    let config = read_json(&f.path("home/.zcode/cli/config.json"));
    assert_eq!(
        config["plugins"]["enabledPlugins"],
        json!({"base@mkt": true, "kit@mkt": true})
    );
    let list = json_out(&f.run(&["list", "--json"]).await);
    let kit = list
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "kit@mkt")
        .unwrap();
    assert_eq!(kit["source"], "cache");
    assert_eq!(kit["skillCount"], 1);
    assert!(
        kit["rootPath"]
            .as_str()
            .unwrap()
            .contains("/cache/mkt/kit/1.0.0-")
    );
    let available = json_out(&f.run(&["list", "--available", "--json"]).await);
    assert_eq!(
        available["available"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["installed"] == true)
            .count(),
        2
    );

    // 启停：用户级与项目层；local 照 Node 报错。
    let disabled = f.run(&["disable", "kit"]).await;
    assert!(
        disabled.stdout.starts_with("Disabled plugin kit@mkt in "),
        "{disabled:?}"
    );
    let enabled = f.run(&["enable", "kit@mkt", "-s", "project"]).await;
    assert_eq!(enabled.code, 0, "{enabled:?}");
    assert_eq!(
        read_json(&f.path("work/.zcode/config.json"))["plugins"]["enabledPlugins"]["kit@mkt"],
        true
    );
    let local = f.run(&["enable", "kit", "-s", "local"]).await;
    assert_eq!(local.code, 1);
    assert!(local.stderr.starts_with(
        "Scope 'local' is not supported by zcode. Use: user, project\nUsage: zcode plugin"
    ));

    // 更新：先刷新市场，再装新版本；旧目录打孤儿标记。
    let old_root = PathBuf::from(kit["rootPath"].as_str().unwrap());
    f.market("1.1.0");
    let updated = f.run(&["update", "kit"]).await;
    assert_eq!(
        updated.stdout, "Updated plugin kit@mkt from 1.0.0 to 1.1.0. Restart zcode to apply.\n",
        "{updated:?}"
    );
    assert!(old_root.join(".orphaned_at").is_file());
    let again = f.run(&["update", "kit@mkt"]).await;
    assert_eq!(
        again.stdout,
        "Plugin kit@mkt is already up to date (1.1.0).\n"
    );

    // 卸载：非交互不带 --force 拒绝；--keep-data 留数据目录；再卸一次说没装。
    let refused = f.run(&["uninstall", "kit"]).await;
    assert_eq!(refused.code, 1);
    assert!(
        refused
            .stderr
            .starts_with("Refusing to uninstall kit@mkt without confirmation.")
    );
    let aborted = f.run_with(&["uninstall", "kit"], true).await;
    assert_eq!(aborted.stdout, "Aborted. kit@mkt was not uninstalled.\n");
    std::fs::create_dir_all(f.store("data/kit@mkt")).unwrap();
    let removed = json_out(
        &f.run(&["uninstall", "kit@mkt", "--force", "--keep-data", "--json"])
            .await,
    );
    assert_eq!(removed["removed"], true);
    assert_eq!(removed["keptData"], true);
    assert!(f.store("data/kit@mkt").is_dir());
    let config = read_json(&f.path("home/.zcode/cli/config.json"));
    assert!(config["plugins"]["enabledPlugins"].get("kit@mkt").is_none());
    let twice = f.run(&["uninstall", "kit@mkt", "--force"]).await;
    assert_eq!(
        (twice.code, twice.stdout.as_str()),
        (1, "Plugin not installed: kit@mkt\n")
    );

    let gone = f.run(&["marketplace", "remove", "mkt"]).await;
    assert_eq!(gone.stdout, "Removed marketplace mkt\n");
}

/// 本机 HTTP 服务：每个请求回同一份字节，返回地址。
async fn serve(body: Vec<u8>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let body = body.clone();
            tokio::spawn(async move {
                let mut request = [0_u8; 4096];
                let _ = socket.read(&mut request).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&body).await;
            });
        }
    });
    format!("http://{address}")
}

fn zip(entries: &[(&str, &str)]) -> Vec<u8> {
    use std::io::Write;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, text) in entries {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(text.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// 市场清单里这个 id 的那一项。
fn marketplace<'a>(listed: &'a Value, id: &str) -> Option<&'a Value> {
    listed.as_array().unwrap().iter().find(|m| m["id"] == id)
}

/// 已知市场的记录里有没有这个 id（`known_marketplaces.json` 的 `marketplaces`，没有文件时没有）。
fn known(f: &Fixture, id: &str) -> bool {
    let path = f.store("known_marketplaces.json");
    path.exists()
        && read_json(&path)["marketplaces"]
            .as_array()
            .is_some_and(|records| records.iter().any(|record| record["id"] == id))
}

/// 用户级配置声明 `mkt`（相对路径相对用户配置所在的目录 `home/.zcode/cli`），项目层也声明一个（不读）。
fn declare(f: &Fixture, path: &str) {
    write(
        &f.path("home/.zcode/cli/config.json"),
        json!({"plugins": {"extraKnownMarketplaces": {
            "mkt": {"source": {"source": "directory", "path": path}}
        }}})
        .to_string(),
    );
    write(
        &f.path("work/.zcode/config.json"),
        json!({"plugins": {"extraKnownMarketplaces": {
            "proj": {"source": {"source": "directory", "path": "/Users/dev/proj"}}
        }}})
        .to_string(),
    );
}

#[tokio::test]
async fn declared_marketplaces_are_listed_and_materialize_only_on_install() {
    let f = Fixture::new(&[]).await;
    f.market("1.0.0");
    declare(&f, "../../../mkt");
    // 列出：用户级的声明在（没有清单：0 个插件、名字是 id），项目层的不在；不写已知市场的记录
    let listed = json_out(&f.run(&["marketplace", "list", "--json"]).await);
    let mkt = marketplace(&listed, "mkt").unwrap_or_else(|| panic!("{listed}"));
    assert_eq!(mkt["pluginCount"], 0, "{mkt}");
    assert_eq!(mkt["name"], "mkt");
    assert!(marketplace(&listed, "proj").is_none(), "{listed}");
    assert!(!known(&f, "mkt"));
    // 安装：先按声明加市场，再装
    let installed = json_out(&f.run(&["install", "kit@mkt", "--json"]).await);
    assert_eq!(installed["ok"], true, "{installed}");
    assert!(known(&f, "mkt"));
    let listed = json_out(&f.run(&["marketplace", "list", "--json"]).await);
    assert_eq!(
        marketplace(&listed, "mkt").unwrap()["pluginCount"],
        2,
        "{listed}"
    );
}

#[tokio::test]
async fn an_explicit_refresh_materializes_a_declaration_and_a_repointed_one_is_refused() {
    let f = Fixture::new(&[]).await;
    f.market("1.0.0");
    declare(&f, "../../../mkt");
    let refreshed = json_out(&f.run(&["marketplace", "update", "mkt", "--json"]).await);
    assert_eq!(refreshed["marketplaces"][0]["id"], "mkt", "{refreshed}");
    assert!(known(&f, "mkt"));
    // 声明改指到别的目录：已知记录的来源不同，显式刷新与安装都拒绝（不改指）
    std::fs::rename(f.path("mkt"), f.path("mkt-moved")).unwrap();
    declare(&f, "../../../mkt-moved");
    let refused = f.run(&["marketplace", "update", "mkt", "--json"]).await;
    assert_eq!(refused.code, 1, "{refused:?}");
    let refused: Value = serde_json::from_str(&refused.stdout).unwrap();
    assert!(
        refused["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("conflicts with an existing Host source"),
        "{refused}"
    );
    let install = f.run(&["install", "kit@mkt", "--json"]).await;
    assert_eq!(install.code, 1, "{install:?}");
    assert!(
        install
            .stdout
            .contains("conflicts with an existing Host source"),
        "{install:?}"
    );
}

#[tokio::test]
async fn a_zip_marketplace_installs_from_a_local_server() {
    let f = Fixture::new(&[]).await;
    let bytes = zip(&[
        (
            "kit/.claude-plugin/plugin.json",
            r#"{"name":"zipkit","version":"3.0.0"}"#,
        ),
        ("kit/skills/z/SKILL.md", "---\ndescription: z\n---\n"),
    ]);
    let sha = rust_cli_base::hash::sha256_hex(&bytes);
    let base = serve(bytes).await;
    write(
        &f.path("zips.json"),
        json!({"name": "zips", "plugins": [
            {"name": "zipkit", "source": {"source": "url", "type": "zip", "url": format!("{base}/kit.zip"), "sha256": sha}}
        ]})
        .to_string(),
    );
    let mkt = f.path("zips.json").display().to_string();
    assert_eq!(
        f.run(&["marketplace", "add", &mkt]).await.stdout,
        "Added marketplace zips (1 plugins)\n"
    );
    let installed = json_out(&f.run(&["install", "zipkit@zips", "--json"]).await);
    assert_eq!(installed["ok"], true);
    assert_eq!(installed["installed"][0]["version"], "3.0.0");
    let list = json_out(&f.run(&["list", "--json"]).await);
    assert_eq!(list[0]["id"], "zipkit@zips");
}

#[tokio::test]
async fn validate_and_refresh_failures_report_diagnostics() {
    let f = Fixture::new(&[]).await;
    f.market("1.0.0");
    let mkt = f.path("mkt").display().to_string();
    assert_eq!(
        f.run(&["validate", &mkt]).await.stdout,
        format!("Plugin manifest is valid: {mkt}\n")
    );
    let missing = json_out_any(&f.run(&["validate", "nope", "--json"]).await);
    assert_eq!(missing["ok"], false);
    f.run(&["marketplace", "add", &mkt]).await;
    std::fs::remove_dir_all(f.path("mkt")).unwrap();
    let failed = f.run(&["marketplace", "update", "mkt"]).await;
    assert_eq!(failed.code, 1);
    assert!(
        failed
            .stderr
            .starts_with("Marketplace update failed\n- [error] mkt plugin_marketplace_invalid:"),
        "{failed:?}"
    );
    let known = read_json(&f.store("known_marketplaces.json"));
    let record = known["marketplaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "mkt")
        .unwrap()
        .clone();
    assert!(record["lastRefreshFailure"]["message"].is_string());
}

fn json_out_any(output: &Output) -> Value {
    serde_json::from_str(&output.stdout).unwrap()
}

/// 内置插件（plugin-store.md §8）：卸载写 `suppressedBuiltins`；安装就是恢复；从官方市场装的同名插件生效（A15）。
#[tokio::test]
async fn builtins_are_suppressed_restored_and_overridden_by_installs() {
    let dir = tempfile::tempdir().unwrap();
    let builtin = dir.path().join("builtin");
    write(
        &builtin.join("pdf/.zcode-plugin/plugin.json"),
        json!({"name": "pdf", "version": "1.0.0"}).to_string(),
    );
    write(
        &builtin.join("pdf/skills/pdf/SKILL.md"),
        "---\ndescription: pdf\n---\n",
    );
    let f = Fixture::new(&[("ZCODE_BUILTIN_PLUGINS_DIR", builtin.display().to_string())]).await;
    let list = json_out(&f.run(&["list", "--json"]).await);
    assert_eq!(list[0]["source"], "official");
    assert_eq!(list[0]["enabled"], true);

    let removed = f.run(&["uninstall", "pdf", "--force"]).await;
    assert_eq!(
        removed.stdout, "Uninstalled plugin pdf@zcode-plugins-official\n",
        "{removed:?}"
    );
    let config = read_json(&f.path("home/.zcode/cli/config.json"));
    assert_eq!(
        config["plugins"]["suppressedBuiltins"],
        json!(["pdf@zcode-plugins-official"])
    );
    assert_eq!(json_out(&f.run(&["list", "--json"]).await), json!([]));
    let available = json_out(&f.run(&["list", "--available", "--json"]).await);
    assert_eq!(available["available"], json!([]));

    let restored = f.run(&["install", "pdf@zcode-plugins-official"]).await;
    assert_eq!(restored.code, 0, "{restored:?}");
    let config = read_json(&f.path("home/.zcode/cli/config.json"));
    assert_eq!(config["plugins"]["suppressedBuiltins"], json!([]));

    // 官方市场的清单缓存（rust 自己的文件）里有新版本：装上之后它生效，内置的那份被盖过。
    write(
        &f.path("cdn/pdf/.claude-plugin/plugin.json"),
        json!({"name": "pdf", "version": "2.0.0"}).to_string(),
    );
    write(
        &f.store("rust-official-marketplace.json"),
        json!({"name": "zcode-plugins-official", "plugins": [
            {"name": "pdf", "source": {"source": "directory", "path": f.path("cdn/pdf").display().to_string()}}
        ]})
        .to_string(),
    );
    let installed = f.run(&["install", "pdf@zcode-plugins-official"]).await;
    assert_eq!(
        installed.stdout, "Installed plugin pdf@zcode-plugins-official (2.0.0) [enabled]\n",
        "{installed:?}"
    );
    let list = json_out(&f.run(&["list", "--json"]).await);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["version"], "2.0.0");
    assert_eq!(list[0]["source"], "cache");
}

/// 配置值（服务的 `configure`，PLUG-P4A 的端口接它）：敏感键进私有文件，普通键进 config.json，旧的敏感明文迁走。
#[tokio::test]
async fn configure_splits_sensitive_values_into_the_private_file() {
    let f = Fixture::new(&[]).await;
    write(
        &f.path("work/kit/.claude-plugin/plugin.json"),
        json!({"name": "kit", "userConfig": {"token": {"type": "string", "sensitive": true}, "color": {"type": "string"}}}).to_string(),
    );
    write(
        &f.path("home/.zcode/cli/config.json"),
        json!({"plugins": {"dirs": ["kit"], "options": {"kit@inline": {"token": "old-plain"}}}})
            .to_string(),
    );
    let control = Control::new(CancellationToken::new());
    let set = json!({"color": "blue"});
    f.admin
        .configure(
            &f.cwd,
            "kit",
            Scope::User,
            (set.as_object().unwrap(), &[]),
            &control,
        )
        .await
        .unwrap();
    let config = read_json(&f.path("home/.zcode/cli/config.json"));
    assert_eq!(
        config["plugins"]["options"]["kit@inline"],
        json!({"color": "blue"})
    );
    let secrets = read_json(&f.path("home/.zcode/cli/plugin-secrets.json"));
    assert_eq!(secrets["user"]["kit@inline"], json!({"token": "old-plain"}));
    let set = json!({"token": "s3cret"});
    f.admin
        .configure(
            &f.cwd,
            "kit",
            Scope::Workspace,
            (set.as_object().unwrap(), &[]),
            &control,
        )
        .await
        .unwrap();
    let secrets = read_json(&f.path("home/.zcode/cli/plugin-secrets.json"));
    let key = rust_cli_plugin_store::core::secrets::project_key(&f.cwd);
    assert_eq!(
        secrets["workspaces"][key.as_str()]["kit@inline"],
        json!({"token": "s3cret"})
    );
    assert_eq!(
        read_json(&f.path("work/.zcode/config.json"))["plugins"]["options"]["kit@inline"],
        json!({})
    );
}

#[test]
fn parsing_follows_node_flags() {
    let args = |list: &[&str]| list.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
    assert!(parse(&args(&["hooks"])).is_none());
    let command = parse(&args(&[
        "plugins",
        "marketplace",
        "add",
        "x",
        "--sparse",
        "a",
        "--sparse=b",
        "-s",
        "user",
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(command.args, ["marketplace", "add", "x"]);
    assert_eq!(command.flags.sparse, ["a", "b"]);
    assert_eq!(command.flags.scope.as_deref(), Some("user"));
    let error = parse(&args(&["plugin", "list", "--bogus"]))
        .unwrap()
        .unwrap_err();
    assert!(error.starts_with("Unknown option '--bogus'\nUsage: zcode plugin"));
}
