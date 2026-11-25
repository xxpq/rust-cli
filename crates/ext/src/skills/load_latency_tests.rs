//! 发现的往返次数（ext.md §2.1 末条）：每次 Files 调用有固定延迟（远端机器的一次往返）时，发现的耗时按“串起来的
//! 调用层数”算，不按调用总数。时间用 tokio 的暂停时钟，结果是确定的。

use std::sync::Arc;

use rust_cli_kernel_api::testing::MemoryEnvironment;

use super::*;
use crate::test_support::{CWD, HOME, RTT, Slow, facts, workspace};

fn skill(dir: &str, name: &str) -> (String, String) {
    (
        format!("{dir}/{name}/SKILL.md"),
        format!("---\ndescription: {name}\n---\nbody\n"),
    )
}

/// 用户级 20 个、项目级 20 个 skill，命令目录 2 层共 6 个命令。
fn many() -> MemoryEnvironment {
    let mut env = MemoryEnvironment::new(facts());
    for index in 0..20 {
        for (path, text) in [
            skill(
                &format!("{HOME}/.zcode/skills"),
                &format!("user-{index:02}"),
            ),
            skill(
                &format!("{CWD}/.agents/skills"),
                &format!("project-{index:02}"),
            ),
        ] {
            env = env.with_file(&path, &text);
        }
    }
    for name in [
        "a",
        "b",
        "nested/c",
        "nested/d",
        "nested/deeper/e",
        "nested/deeper/f",
    ] {
        env = env.with_file(&format!("{CWD}/.zcode/commands/{name}.md"), "run it\n");
    }
    env
}

#[tokio::test(start_paused = true)]
async fn discovery_time_follows_the_call_depth_not_the_call_count() {
    let info = Arc::new(MemoryEnvironment::new(facts()));
    let catalog = SkillCatalog::new(info, Arc::new(Slow(many())));
    let started = tokio::time::Instant::now();
    let skills = catalog.skills(&workspace(), &[]).await;
    let first = started.elapsed();
    let found = skills
        .iter()
        .filter(|skill| skill.source != SkillSource::Builtin)
        .count();
    assert_eq!(found, 46);
    // 串起来的层：项目层级（cwd 的 `.git` 一次）→ 命令目录 3 层列目录 → 一次 stat_many → 读没缓存的。
    // 以前逐个调用：首次 1.06 s，之后每一步 600 ms。
    assert!(first <= RTT * 6, "first discovery took {first:?}");
    // 第二步：解析结果都在缓存里，只剩列目录与 stat。
    let started = tokio::time::Instant::now();
    catalog.skills(&workspace(), &[]).await;
    let again = started.elapsed();
    assert!(again <= RTT * 5, "a later step took {again:?}");
}
