//! 插件来源与启用判定（ext.md §4.3、§4.4）。

use std::collections::BTreeMap;

use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::testing::MemoryEnvironment;

use super::*;
use crate::test_support::{facts, workspace};

const OFFICIAL: &str = "zcode-plugins-official";
const CACHE: &str = "/Users/dev/.zcode/cli/plugins/cache";

fn manifest(dir: &str, body: &str) -> (String, String) {
    (format!("{dir}/.zcode-plugin/plugin.json"), body.to_owned())
}

fn env(files: &[(String, String)]) -> MemoryEnvironment {
    files
        .iter()
        .fold(MemoryEnvironment::new(facts()), |env, (path, text)| {
            env.with_file(path, text)
        })
}

fn candidate(dir: &str, marketplace: &str, origin: PluginOrigin) -> PluginCandidate {
    PluginCandidate {
        dir: EnvPath::new(dir),
        marketplace: marketplace.to_owned(),
        origin,
    }
}

fn official(name: &str) -> PluginCandidate {
    candidate(
        &format!("{CACHE}/{OFFICIAL}/{name}/1.0.0"),
        OFFICIAL,
        PluginOrigin::Official,
    )
}

fn official_manifest(name: &str, extra: &str) -> (String, String) {
    manifest(
        &format!("{CACHE}/{OFFICIAL}/{name}/1.0.0"),
        &format!(r#"{{"name": "{name}"{extra}}}"#),
    )
}

/// 官方表（入口层的数据）：缺省启用、依赖 node_repl、依赖 Node 运行时的 id。
fn table() -> OfficialTable {
    let ids = |names: &[&str]| {
        names
            .iter()
            .map(|name| format!("{name}@{OFFICIAL}"))
            .collect()
    };
    OfficialTable {
        default_enabled: ids(&[
            "skill-creator",
            "zcode-guide",
            "browser-use",
            "documents",
            "image-search",
            "pdf",
        ]),
        node_repl: ids(&["browser-use", "computer-use"]),
        node_runtime: ids(&["documents", "android-emulator"]),
    }
}

/// 入口给的原因（日志里写它）。
const UNHOSTED_WHY: &str = "the frontend cannot show it";

/// 入口交来的「撑不起来的插件」：官方市场的 views（哪些插件要什么前端能力，表在入口，context.md §15.7）。
fn unhosted_in_the_terminal() -> Unhosted {
    BTreeMap::from([(format!("views@{OFFICIAL}"), UNHOSTED_WHY.to_owned())])
}

fn enabled(pairs: &[(&str, bool)]) -> BTreeMap<String, bool> {
    pairs
        .iter()
        .map(|(id, on)| ((*id).to_owned(), *on))
        .collect()
}

fn names(loaded: &PluginSet) -> Vec<&str> {
    loaded.plugins().iter().map(Plugin::name).collect()
}

fn reasons(loaded: &PluginSet) -> Vec<(Option<&str>, &SkipReason)> {
    loaded
        .skipped()
        .iter()
        .map(|skipped| (skipped.id.as_deref(), &skipped.reason))
        .collect()
}

#[tokio::test]
async fn enablement_follows_enabled_plugins_and_the_defaults() {
    let files = [
        manifest("/Users/dev/cfg", r#"{"name": " cfg-kit "}"#),
        (
            "/Users/dev/cfg/skills/hello/SKILL.md".to_owned(),
            "Hello\n".to_owned(),
        ),
        manifest("/Users/dev/off", r#"{"name": "off"}"#),
        manifest("/Users/dev/market-kit", r#"{"name": "market-kit"}"#),
        manifest("/Users/dev/on-kit", r#"{"name": "on-kit"}"#),
        official_manifest("skill-creator", ""),
        official_manifest("restore-legacy-sessions", ""),
        official_manifest("zcode-guide", ""),
    ];
    let sources = PluginSources {
        candidates: vec![
            candidate("/Users/dev/cfg", "inline", PluginOrigin::ConfigDir),
            candidate("/Users/dev/off", "inline", PluginOrigin::ConfigDir),
            official("skill-creator"),
            official("restore-legacy-sessions"),
            official("zcode-guide"),
            candidate("/Users/dev/market-kit", "market", PluginOrigin::Installed),
            candidate("/Users/dev/on-kit", "market", PluginOrigin::Installed),
        ],
        enabled_plugins: enabled(&[("off@inline", false), ("on-kit@market", true)]),
        suppressed_builtins: vec![format!("zcode-guide@{OFFICIAL}")],
        official: table(),
        ..PluginSources::default()
    };
    let loaded = load(
        &env(&files),
        &workspace(),
        &[],
        (&sources, &Unhosted::default()),
    )
    .await;
    // 配置目录缺省启用；官方插件只有缺省名单里的启用；已安装插件要 `enabledPlugins` 打开。
    assert_eq!(names(&loaded), ["cfg-kit", "skill-creator", "on-kit"]);
    assert_eq!(
        loaded.plugins()[0].skill_dirs,
        ["/Users/dev/cfg/skills".to_owned()]
    );
    assert_eq!(
        reasons(&loaded),
        [
            (Some("off@inline"), &SkipReason::Disabled),
            (
                Some("restore-legacy-sessions@zcode-plugins-official"),
                &SkipReason::Disabled
            ),
            (
                Some("zcode-guide@zcode-plugins-official"),
                &SkipReason::Suppressed
            ),
            (Some("market-kit@market"), &SkipReason::Disabled),
        ]
    );
}

#[tokio::test]
async fn sources_other_than_the_command_line_need_a_valid_manifest() {
    let files = [
        (
            "/Users/dev/bare/skills/x/SKILL.md".to_owned(),
            "B\n".to_owned(),
        ),
        manifest("/Users/dev/bad", r#"{"name": "Bad Name"}"#),
        (
            "/Users/dev/other/.codex-plugin/plugin.json".to_owned(),
            r#"{"name": "other-kit"}"#.to_owned(),
        ),
        (
            "/Users/dev/session/skills/y/SKILL.md".to_owned(),
            "S\n".to_owned(),
        ),
    ];
    let sources = PluginSources {
        candidates: vec![
            candidate("/Users/dev/bare", "inline", PluginOrigin::ConfigDir),
            candidate("/Users/dev/bad", "inline", PluginOrigin::ConfigDir),
            candidate("/Users/dev/other", "inline", PluginOrigin::ConfigDir),
            candidate("/Users/dev/missing", "inline", PluginOrigin::ConfigDir),
        ],
        ..PluginSources::default()
    };
    let session = [EnvPath::new("/Users/dev/session")];
    let loaded = load(
        &env(&files),
        &workspace(),
        &session,
        (&sources, &Unhosted::default()),
    )
    .await;
    // `--plugin-dir` 没有 manifest 时取目录名；其余来源按 Node 必须有合法的 manifest。
    assert_eq!(names(&loaded), ["session", "other-kit"]);
    let reasons: Vec<&SkipReason> = loaded.skipped().iter().map(|s| &s.reason).collect();
    assert!(matches!(
        reasons.as_slice(),
        [
            SkipReason::ManifestMissing,
            SkipReason::ManifestInvalid(_),
            SkipReason::RootMissing
        ]
    ));
}

#[tokio::test]
async fn session_plugins_and_first_comers_win_name_conflicts() {
    let files = [
        manifest("/Users/dev/session-dup", r#"{"name": "dup"}"#),
        manifest("/Users/dev/cfg-dup", r#"{"name": "dup"}"#),
        manifest("/Users/dev/twin-a", r#"{"name": "twin"}"#),
        manifest("/Users/dev/twin-b", r#"{"name": "twin"}"#),
        manifest("/Users/dev/twin-market", r#"{"name": "twin"}"#),
    ];
    let sources = PluginSources {
        candidates: vec![
            candidate("/Users/dev/cfg-dup", "inline", PluginOrigin::ConfigDir),
            candidate("/Users/dev/twin-a", "inline", PluginOrigin::ConfigDir),
            candidate("/Users/dev/twin-b", "inline", PluginOrigin::ConfigDir),
            candidate("/Users/dev/twin-market", "market", PluginOrigin::Installed),
        ],
        enabled_plugins: enabled(&[("twin@market", true)]),
        ..PluginSources::default()
    };
    let session = [EnvPath::new("/Users/dev/session-dup")];
    let loaded = load(
        &env(&files),
        &workspace(),
        &session,
        (&sources, &Unhosted::default()),
    )
    .await;
    assert_eq!(names(&loaded), ["dup", "twin"]);
    assert_eq!(loaded.plugins()[1].dir(), "/Users/dev/twin-a");
    assert_eq!(
        reasons(&loaded),
        [
            (Some("dup@inline"), &SkipReason::ShadowedBySession),
            (Some("twin@inline"), &SkipReason::DuplicateId),
            (Some("twin@market"), &SkipReason::DuplicateName),
        ]
    );
}

/// 依赖 Node 的官方插件（ext.md §4.4）：`node_repl` 与 Node 运行时按官方表的 id，官方鉴权按 MCP 的 `auth`；只判定官方市场的
/// 插件。PLUG-P0：以前 Node 运行时按缓存里改写的 `__zcode-plugin-host` 参数判断，安装包里的原样插件（`command: node`）判断不出。
#[tokio::test]
async fn bundled_plugins_that_need_the_node_runtime_are_skipped() {
    let node_mcp = r#", "mcpServers": {"documents": {"type": "stdio", "command": "node", "args": ["${ZCODE_PLUGIN_ROOT}/dist/server.mjs"]}}"#;
    let files = [
        official_manifest("browser-use", r#", "skills": "skills""#),
        official_manifest("computer-use", ""),
        official_manifest("documents", node_mcp),
        official_manifest("image-search", ""),
        (
            format!("{CACHE}/{OFFICIAL}/image-search/1.0.0/.mcp.json"),
            r#"{"mcpServers": {"image_search": {"type": "http", "url": "${ZCODE_BASE_URL}/mcp", "auth": {"type": "zcode_official", "provider": "jwt_token"}}}}"#.to_owned(),
        ),
        official_manifest("views", ""),
        official_manifest("android-emulator", node_mcp),
        official_manifest("skill-creator", ""),
        manifest(
            "/Users/dev/third",
            &format!(r#"{{"name": "third"{node_mcp}}}"#),
        ),
    ];
    let sources = PluginSources {
        candidates: vec![
            official("browser-use"),
            official("computer-use"),
            official("documents"),
            official("image-search"),
            official("views"),
            official("android-emulator"),
            official("skill-creator"),
            candidate("/Users/dev/third", "market", PluginOrigin::Installed),
        ],
        enabled_plugins: enabled(&[
            ("computer-use@zcode-plugins-official", true),
            ("views@zcode-plugins-official", true),
            ("third@market", true),
        ]),
        official: table(),
        ..PluginSources::default()
    };
    let loaded = load(
        &env(&files),
        &workspace(),
        &[],
        (&sources, &unhosted_in_the_terminal()),
    )
    .await;
    // 只判定官方市场的插件：第三方插件里同样的 MCP 照常加载。
    assert_eq!(names(&loaded), ["skill-creator", "third"]);
    assert_eq!(
        reasons(&loaded),
        [
            (
                Some("browser-use@zcode-plugins-official"),
                &SkipReason::NodeRuntime(NodeDependency::NodeRepl)
            ),
            (
                Some("computer-use@zcode-plugins-official"),
                &SkipReason::NodeRuntime(NodeDependency::NodeRepl)
            ),
            (
                Some("documents@zcode-plugins-official"),
                &SkipReason::NodeRuntime(NodeDependency::PluginHost)
            ),
            (
                Some("image-search@zcode-plugins-official"),
                &SkipReason::NodeRuntime(NodeDependency::OfficialAuth)
            ),
            // 前端撑不起来（context.md §15.7）：入口交来的原因
            (
                Some("views@zcode-plugins-official"),
                &SkipReason::FrontendLacks(UNHOSTED_WHY.to_owned())
            ),
            // 未启用的插件只记“未启用”，不判定 Node 依赖。
            (
                Some("android-emulator@zcode-plugins-official"),
                &SkipReason::Disabled
            ),
        ]
    );
}

/// 前端撑不起来的插件（context.md §15.7，A6）：入口交来的表里的 id 整件不加载，日志写入口给的原因；表里没有时照常加载。ext 只按
/// id 查：不是官方市场的同名插件（会话插件 `views@inline`）不受这张表管。表与能力的种类在入口；这里由 `enabledPlugins` 打开它。
#[tokio::test]
async fn unhosted_plugins_are_skipped_with_the_given_reason() {
    let files = [
        official_manifest("views", r#", "skills": "skills""#),
        manifest("/Users/dev/views", r#"{"name": "views"}"#),
    ];
    let sources = PluginSources {
        candidates: vec![official("views")],
        enabled_plugins: enabled(&[("views@zcode-plugins-official", true)]),
        official: table(),
        ..PluginSources::default()
    };
    let hosted = load(
        &env(&files),
        &workspace(),
        &[],
        (&sources, &Unhosted::default()),
    )
    .await;
    assert_eq!(names(&hosted), ["views"]);
    let terminal = load(
        &env(&files),
        &workspace(),
        &[],
        (&sources, &unhosted_in_the_terminal()),
    )
    .await;
    assert!(names(&terminal).is_empty());
    assert_eq!(
        reasons(&terminal),
        [(
            Some("views@zcode-plugins-official"),
            &SkipReason::FrontendLacks(UNHOSTED_WHY.to_owned())
        )]
    );
    // 会话插件（`--plugin-dir`）叫这个名字也不按表判
    let session = [EnvPath::new("/Users/dev/views")];
    let own = load(
        &env(&files),
        &workspace(),
        &session,
        (
            &PluginSources {
                official: table(),
                ..PluginSources::default()
            },
            &unhosted_in_the_terminal(),
        ),
    )
    .await;
    assert_eq!(names(&own), ["views"]);
}

/// 内置插件与用户从官方市场装的同 id 插件（A15，plugin-store.md §8.2）：入口把装的排在前面，装的生效，内置的以
/// `InstalledOverride` 跳过（不警告）。
#[tokio::test]
async fn an_installed_official_plugin_overrides_the_builtin() {
    let installed =
        "/Users/dev/.zcode/cli/plugins/cache/zcode-plugins-official/pdf/2.0.0-abcdef012345";
    let files = [
        manifest(installed, r#"{"name": "pdf", "version": "2.0.0"}"#),
        official_manifest("pdf", r#", "version": "1.0.0""#),
    ];
    let sources = PluginSources {
        candidates: vec![
            candidate(installed, OFFICIAL, PluginOrigin::Installed),
            official("pdf"),
        ],
        official: table(),
        ..PluginSources::default()
    };
    let loaded = load(
        &env(&files),
        &workspace(),
        &[],
        (&sources, &Unhosted::default()),
    )
    .await;
    assert_eq!(names(&loaded), ["pdf"]);
    assert_eq!(loaded.plugins()[0].dir(), installed);
    assert_eq!(
        reasons(&loaded),
        [(
            Some("pdf@zcode-plugins-official"),
            &SkipReason::InstalledOverride
        )]
    );
}
