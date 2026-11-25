//! 有文件变化的订阅时记下发现的结果（ext.md §2.1.1）：没有变化的步不读文件；有变化、订阅失效时重新发现。

use rust_cli_base::sync::LockUnpoisoned;
use std::sync::{Arc, Mutex};

use futures_util::stream::BoxStream;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::file_watch::{FileWatch, WatchRequest};
use rust_cli_kernel_api::testing::MemoryEnvironment;
use tokio::sync::mpsc;

use super::*;
use crate::test_support::{CWD, HOME, facts, workspace};

/// 记下每次订阅；测试经发送端送变化，放下发送端就是订阅失效。`supported` 为 false 时监听不了。
struct FakeWatch {
    supported: bool,
    subscriptions: Mutex<Vec<(WatchRequest, mpsc::UnboundedSender<()>)>>,
}

impl FakeWatch {
    fn new(supported: bool) -> Arc<Self> {
        Arc::new(Self {
            supported,
            subscriptions: Mutex::default(),
        })
    }

    fn count(&self) -> usize {
        self.subscriptions.lock_unpoisoned().len()
    }

    fn change(&self) {
        let subscriptions = self.subscriptions.lock_unpoisoned();
        if let Some((_, sender)) = subscriptions.last() {
            let _ = sender.send(());
        }
    }

    fn end(&self) {
        self.subscriptions.lock_unpoisoned().clear();
    }

    fn dirs(&self) -> Vec<String> {
        self.subscriptions.lock_unpoisoned()[0]
            .0
            .dirs
            .iter()
            .map(|dir| dir.as_str().to_owned())
            .collect()
    }
}

impl FileWatch for FakeWatch {
    fn subscribe(
        &self,
        _workspace: &WorkspaceRef,
        request: WatchRequest,
    ) -> Option<BoxStream<'static, ()>> {
        if !self.supported {
            return None;
        }
        let (sender, receiver) = mpsc::unbounded_channel();
        self.subscriptions.lock_unpoisoned().push((request, sender));
        Some(Box::pin(futures_util::stream::unfold(
            receiver,
            |mut receiver| async move { receiver.recv().await.map(|item| (item, receiver)) },
        )))
    }
}

fn file(name: &str) -> (String, String) {
    (
        format!("{CWD}/.zcode/skills/{name}/SKILL.md"),
        format!("---\ndescription: {name}\n---\nbody\n"),
    )
}

fn names(skills: &[Arc<Skill>]) -> Vec<String> {
    skills
        .iter()
        .filter(|skill| skill.source != SkillSource::Builtin)
        .map(|skill| skill.name.clone())
        .collect()
}

fn catalog(watch: Arc<FakeWatch>) -> (Arc<MemoryEnvironment>, SkillCatalog) {
    let (path, text) = file("alpha");
    let env = Arc::new(MemoryEnvironment::new(facts()).with_file(&path, &text));
    let catalog = SkillCatalog::new(env.clone(), env.clone()).with_watch(Some(watch));
    (env, catalog)
}

#[tokio::test]
async fn an_unchanged_step_reads_no_files() {
    let watch = FakeWatch::new(true);
    let (env, catalog) = catalog(watch.clone());
    assert_eq!(names(&catalog.skills(&workspace(), &[]).await), ["alpha"]);
    assert_eq!(watch.count(), 1);
    // 订阅的是 skill 与命令的根目录（还不存在的也在里面）。
    let dirs = watch.dirs();
    for dir in [
        format!("{CWD}/.zcode/skills"),
        format!("{CWD}/.agents/commands"),
        format!("{HOME}/.zcode/commands"),
    ] {
        assert!(dirs.contains(&dir), "{dirs:?}");
    }
    let before = env.stat_calls();
    // 文件变了，但订阅没有送变化：用记下的结果。
    let (path, text) = file("beta");
    env.set_file(&path, &text);
    assert_eq!(names(&catalog.skills(&workspace(), &[]).await), ["alpha"]);
    assert_eq!(env.stat_calls(), before, "no stat without a change");
    // 送一次变化：重新发现，沿用原来的订阅。
    watch.change();
    assert_eq!(
        names(&catalog.skills(&workspace(), &[]).await),
        ["alpha", "beta"]
    );
    assert_eq!(watch.count(), 1);
}

#[tokio::test]
async fn an_ended_subscription_falls_back_and_subscribes_again() {
    let watch = FakeWatch::new(true);
    let (env, catalog) = catalog(watch.clone());
    catalog.skills(&workspace(), &[]).await;
    watch.end();
    let (path, text) = file("beta");
    env.set_file(&path, &text);
    assert_eq!(
        names(&catalog.skills(&workspace(), &[]).await),
        ["alpha", "beta"]
    );
    assert_eq!(
        watch.count(),
        1,
        "the ended one was dropped, a new one taken"
    );
}

#[tokio::test]
async fn other_directories_are_another_memo() {
    let watch = FakeWatch::new(true);
    let (env, catalog) = catalog(watch.clone());
    catalog.skills(&workspace(), &[]).await;
    let extra = EnvPath::new("/Users/dev/extra");
    env.set_file(
        "/Users/dev/extra/.agents/skills/gamma/SKILL.md",
        "---\ndescription: gamma\n---\n",
    );
    assert_eq!(
        names(&catalog.skills(&workspace(), &[extra]).await),
        ["alpha", "gamma"]
    );
    assert_eq!(watch.count(), 2);
}

#[tokio::test]
async fn without_a_watch_every_step_discovers() {
    let watch = FakeWatch::new(false);
    let (env, catalog) = catalog(watch);
    catalog.skills(&workspace(), &[]).await;
    let (path, text) = file("beta");
    env.set_file(&path, &text);
    assert_eq!(
        names(&catalog.skills(&workspace(), &[]).await),
        ["alpha", "beta"]
    );
}
