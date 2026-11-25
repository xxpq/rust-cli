//! `plugins` 段、插件存储根与插件候选（config.md §2.1、ext.md §4.1）。
#![allow(clippy::disallowed_methods, reason = "测试准备临时目录里的插件清单")]

use rust_cli_ext::plugins::PluginOrigin;
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::permission::RuleSource;
use rust_cli_paths::StoragePaths;
use serde_json::{Map, Value, json};

use super::*;

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap()
}

/// 用户级与之后的层（`--settings`）。项目层到合并之前已经只剩 `enabledPlugins`（config.md §1.1，投影见 `files_tests.rs`），
/// 合并本身不按层区分。
fn layers<'a>(user: &'a Map<String, Value>, later: &'a [Map<String, Value>]) -> Vec<Layer<'a>> {
    std::iter::once(Layer {
        source: RuleSource::UserSettings,
        object: user,
        path: "user",
    })
    .chain(later.iter().map(|object| Layer {
        source: RuleSource::CliArg,
        object,
        path: "--settings",
    }))
    .collect()
}

#[test]
fn the_plugins_section_merges_like_node() {
    let user = object(json!({"plugins": {
        "dirs": ["/Users/dev/a", "rel"],
        "enabledPlugins": {"x@m": true, "y@m": true},
        "suppressedBuiltins": ["s1@zcode-plugins-official"],
        "extraKnownMarketplaces": {"team": {"source": {"source": "github", "repo": "example/plugins"}}},
        "options": {"x@m": {"key": "value"}}
    }}));
    let later = [
        object(
            json!({"plugins": {"dirs": ["rel", "/Users/dev/b"], "enabledPlugins": {"y@m": false}}}),
        ),
        object(json!({"plugins": {"suppressedBuiltins": ["s2@zcode-plugins-official"]}})),
    ];
    let mut warnings = Vec::new();
    let merged = merge(&layers(&user, &later), &mut warnings);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(merged.enabled);
    // `dirs` 拼接去重；`enabledPlugins` 按 id 覆盖；`suppressedBuiltins` 由最后写它的层整体替换。
    assert_eq!(merged.dirs, ["/Users/dev/a", "rel", "/Users/dev/b"]);
    assert_eq!(
        merged.enabled_plugins.into_iter().collect::<Vec<_>>(),
        [("x@m".to_owned(), true), ("y@m".to_owned(), false)]
    );
    assert_eq!(merged.suppressed_builtins, ["s2@zcode-plugins-official"]);

    let off = [object(json!({"plugins": {"enabled": false}}))];
    let merged = merge(&layers(&user, &off), &mut warnings);
    assert!(!merged.enabled);
}

#[test]
fn bad_plugin_items_are_skipped_with_warnings() {
    let user = object(json!({"plugins": "all"}));
    let later = [object(json!({"plugins": {
        "enabled": "yes",
        "dirs": ["ok", "", 3],
        "enabledPlugins": {"a@m": true, "b@m": "no"},
        "suppressedBuiltins": "s"
    }}))];
    let mut warnings = Vec::new();
    let merged = merge(&layers(&user, &later), &mut warnings);
    assert!(merged.enabled);
    assert_eq!(merged.dirs, ["ok"]);
    assert_eq!(
        merged.enabled_plugins.into_iter().collect::<Vec<_>>(),
        [("a@m".to_owned(), true)]
    );
    assert!(merged.suppressed_builtins.is_empty());
    assert_eq!(warnings.len(), 6, "{warnings:?}");
    assert!(warnings[0].starts_with("user: plugins must be an object"));
}

fn section(enabled: bool) -> PluginSection {
    PluginSection {
        enabled,
        dirs: vec!["/Users/dev/abs".to_owned(), "rel/kit".to_owned()],
        enabled_plugins: [("kit@market".to_owned(), true)].into_iter().collect(),
        suppressed_builtins: vec!["zcode-guide@zcode-plugins-official".to_owned()],
        option_layers: Vec::new(),
        ..PluginSection::default()
    }
}

fn local_side() -> ExecutionSide {
    ExecutionSide {
        data_root: Some(EnvPath::new("/Users/dev/.zcode/cli/plugins/data")),
        shares_storage: true,
    }
}

/// 读意图（本机实现读 Node 的文件）与算候选分开（ext.md §4.12）：候选按 Node 的顺序；执行侧与插件存储不在同一个文件系统时
/// 不产生内置与已安装插件（以前是 `drop_remote_plugins` 事后删）。
#[tokio::test]
async fn candidates_follow_the_node_order() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let paths = StoragePaths::resolve(&Default::default(), home, home);
    let root = paths.plugin_storage(None, home);
    let guide = root
        .as_path()
        .join("cache/zcode-plugins-official/zcode-guide/0.3.0");
    let partition = root
        .as_path()
        .join("marketplaces/zcode-plugins-official/bundled-marketplace.json");
    std::fs::create_dir_all(partition.parent().unwrap()).unwrap();
    std::fs::write(
        &partition,
        json!({"version": 1, "manifest": {"plugins": [
            {"name": "zcode-guide", "cachePath": guide.to_string_lossy()}
        ]}})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        root.as_path().join("installed_plugins.json"),
        json!({"version": 1, "plugins": [{
            "id": "kit@market", "name": "kit", "marketplace": "market", "version": "1.0.0",
            "installPath": "/Users/dev/kit", "installedAt": "2026-10-01T00:00:00.000Z", "scope": "user"
        }]})
        .to_string(),
    )
    .unwrap();
    let store = LocalPluginStore {
        root: root.clone(),
        secrets: paths.plugin_secrets(None, home),
        builtin: None,
    };
    let config = PluginConfig {
        section: section(true),
        intents: store.intents().await,
        root,
    };
    let sources = sources(&config, "/Users/dev/repo", local_side());
    let candidates: Vec<(String, &str, PluginOrigin)> = sources
        .candidates
        .iter()
        .map(|c| (c.dir.as_str().to_owned(), c.marketplace.as_str(), c.origin))
        .collect();
    assert_eq!(
        candidates,
        [
            (
                "/Users/dev/abs".to_owned(),
                "inline",
                PluginOrigin::ConfigDir
            ),
            (
                "/Users/dev/repo/rel/kit".to_owned(),
                "inline",
                PluginOrigin::ConfigDir
            ),
            (
                guide.to_string_lossy().into_owned(),
                "zcode-plugins-official",
                PluginOrigin::Official
            ),
            (
                "/Users/dev/kit".to_owned(),
                "market",
                PluginOrigin::Installed
            ),
        ]
    );
    assert_eq!(sources.enabled_plugins, config.section.enabled_plugins);
    assert_eq!(
        sources.suppressed_builtins,
        config.section.suppressed_builtins
    );
    assert_eq!(
        sources.data_root.as_ref().map(EnvPath::as_str),
        Some("/Users/dev/.zcode/cli/plugins/data")
    );
    // 官方表是数据：缺省启用照 Node（有 zcode-site、没有 visualize）。
    assert!(
        sources
            .official
            .default_enabled
            .contains("zcode-site@zcode-plugins-official")
    );
    assert!(
        !sources
            .official
            .default_enabled
            .contains("visualize@zcode-plugins-official")
    );

    // 主 env 在别处：配置目录照常，内置与已安装插件不产生。
    let remote = ExecutionSide {
        data_root: None,
        shares_storage: false,
    };
    let origins: Vec<PluginOrigin> = sources_of(&config, remote)
        .candidates
        .iter()
        .map(|c| c.origin)
        .collect();
    assert_eq!(origins, [PluginOrigin::ConfigDir, PluginOrigin::ConfigDir]);

    // `plugins.enabled: false`：配置目录、内置与已安装插件都不加载。
    let off = PluginConfig {
        section: section(false),
        ..config
    };
    assert!(sources_of(&off, local_side()).candidates.is_empty());
}

fn sources_of(config: &PluginConfig, side: ExecutionSide) -> rust_cli_ext::plugins::PluginSources {
    sources(config, "/Users/dev/repo", side)
}

/// App 交来的内置插件目录（ext.md §4.13）：给了就不读 Node 的缓存；读不出时没有内置插件，也不回落到缓存。
#[tokio::test]
async fn the_builtin_directory_replaces_the_node_cache() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let paths = StoragePaths::resolve(&Default::default(), home, home);
    let root = paths.plugin_storage(None, home);
    let cached = root
        .as_path()
        .join("cache/zcode-plugins-official/pdf/1.0.0");
    std::fs::create_dir_all(&cached).unwrap();
    let builtin = home.join("app/plugins");
    std::fs::create_dir_all(builtin.join("pdf")).unwrap();
    std::fs::create_dir_all(builtin.join("documents")).unwrap();
    let store = |builtin: Option<std::path::PathBuf>| LocalPluginStore {
        root: root.clone(),
        secrets: paths.plugin_secrets(None, home),
        builtin,
    };
    assert_eq!(store(None).intents().await.official, [cached]);
    assert_eq!(
        store(Some(builtin.clone())).intents().await.official,
        [builtin.join("documents"), builtin.join("pdf")]
    );
    assert!(
        store(Some(home.join("missing")))
            .intents()
            .await
            .official
            .is_empty()
    );
}

fn layer<'a>(source: RuleSource, path: &'a str, object: &'a Map<String, Value>) -> Layer<'a> {
    Layer {
        source,
        object,
        path,
    }
}

/// 配置值（ext.md §4.9）：一个键从高层往低层找；同一层里私有文件的值优先于明文，是秘密类型；项目层按它的基准目录找私有文件
/// 的 `workspaces` 键（Windows 不分大小写）；没写 `options` 的层也有私有文件的一份。旧代码不读配置值。
#[test]
fn options_take_the_highest_layer_and_the_private_file_first() {
    let user = object(
        json!({"plugins": {"options": {"kit@m": {"color": "blue", "size": 1, "token": "plain-old", "bad": [1]}}}}),
    );
    let project = object(json!({"plugins": {"options": {"kit@m": {"color": "red"}}}}));
    let bare = object(json!({}));
    let layers = [
        layer(
            RuleSource::UserSettings,
            "/Users/dev/.zcode/cli/config.json",
            &user,
        ),
        layer(
            RuleSource::ProjectSettings,
            "/Users/dev/repo/.zcode/config.json",
            &project,
        ),
        layer(
            RuleSource::ProjectSettings,
            "/Users/dev/repo/sub/zcode.json",
            &bare,
        ),
    ];
    let mut warnings = Vec::new();
    let section = merge(&layers, &mut warnings);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(
        section
            .option_layers
            .iter()
            .map(|l| l.secrets.clone())
            .collect::<Vec<_>>(),
        [
            SecretsScope::User,
            SecretsScope::Workspace("/Users/dev/repo".to_owned()),
            SecretsScope::Workspace("/Users/dev/repo/sub".to_owned()),
        ]
    );
    let secret = |text: &str| rust_cli_base::secret::RedactedString::new(text);
    let secrets = PluginSecrets {
        user: [(
            "kit@m".to_owned(),
            [("token".to_owned(), secret("user-token"))]
                .into_iter()
                .collect(),
        )]
        .into_iter()
        .collect(),
        workspaces: vec![(
            "/Users/dev/repo/sub/".to_owned(),
            [(
                "kit@m".to_owned(),
                [("size".to_owned(), secret("9"))].into_iter().collect(),
            )]
            .into_iter()
            .collect(),
        )],
    };
    let options = resolve_options(&section.option_layers, &secrets);
    let kit = &options["kit@m"];
    assert_eq!(kit["color"], PluginOption::Plain("red".to_owned()));
    assert_eq!(kit["token"], PluginOption::Secret(secret("user-token")));
    assert_eq!(kit["size"], PluginOption::Secret(secret("9")));
    assert!(!kit.contains_key("bad"));
    assert_eq!(project_key("C:\\Users\\Dev\\Repo\\"), "c:/users/dev/repo");
    assert_eq!(project_key("/Users/dev/repo/./sub/"), "/Users/dev/repo/sub");
}

#[test]
fn config_dirs_resolve_like_node_path_resolve() {
    // 绝对路径原样；相对路径相对工作区根，`.` 与 `..` 按字面化简；不展开 `~`（Node `resolve`）。
    assert_eq!(
        resolve_dir("/Users/dev/repo", "/Users/dev/kit"),
        "/Users/dev/kit"
    );
    assert_eq!(
        resolve_dir("/Users/dev/repo", "./kit"),
        "/Users/dev/repo/kit"
    );
    assert_eq!(resolve_dir("/Users/dev/repo", "../kit/"), "/Users/dev/kit");
    assert_eq!(
        resolve_dir("/Users/dev/repo", "~/kit"),
        "/Users/dev/repo/~/kit"
    );
}

/// 官方市场的已安装插件排在内置插件之前（A15，plugin-store.md §8.2）；别的已安装插件仍排在内置之后。
#[test]
fn installed_official_plugins_come_before_builtins() {
    let config = PluginConfig {
        section: section(true),
        intents: PluginIntents {
            official: vec![std::path::PathBuf::from("/app/plugins/pdf")],
            installed: vec![
                (
                    "market".to_owned(),
                    std::path::PathBuf::from("/Users/dev/kit"),
                ),
                (
                    "zcode-plugins-official".to_owned(),
                    std::path::PathBuf::from("/Users/dev/pdf-2"),
                ),
            ],
            secrets: Default::default(),
        },
        root: StoragePaths::resolve(
            &Default::default(),
            std::path::Path::new("/h"),
            std::path::Path::new("/h"),
        )
        .plugin_storage(None, std::path::Path::new("/h")),
    };
    let sources = sources(&config, "/Users/dev/repo", local_side());
    let dirs: Vec<(&str, PluginOrigin)> = sources
        .candidates
        .iter()
        .filter(|c| c.origin != PluginOrigin::ConfigDir)
        .map(|c| (c.dir.as_str(), c.origin))
        .collect();
    assert_eq!(
        dirs,
        [
            ("/Users/dev/pdf-2", PluginOrigin::Installed),
            ("/app/plugins/pdf", PluginOrigin::Official),
            ("/Users/dev/kit", PluginOrigin::Installed),
        ]
    );
}
