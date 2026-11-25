//! 关掉 ← 打开任务列表（tui-cockpit.md §3.6）：设置 › General 的开关，存在 `puffin-tui/cockpit.json` 的 `leftArrow`。关掉后
//! 空的输入框上 ← 什么都不做，槽里没有 `← agent cockpit`；`/cockpit` 照旧打开。
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::KeyCode;
use futures_util::future::BoxFuture;
use rust_cli_tui::services::{StoreFile, Stores};

use super::support::{GatedModel, Setup, with_ui};

/// 内存里的 `cockpit.json`：读到的是它现在的文字，写入替换它。
#[derive(Default)]
struct CockpitFile(Mutex<Option<String>>);

/// 写不成的 `cockpit.json`（磁盘满）。
struct FullDisk;

impl Stores for FullDisk {
    fn read(&self, _: StoreFile) -> BoxFuture<'static, Result<Option<String>, String>> {
        Box::pin(async { Ok(None) })
    }

    fn write(&self, _: StoreFile, _: String) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async { Err("disk full".to_owned()) })
    }
}

impl Stores for CockpitFile {
    fn read(&self, file: StoreFile) -> BoxFuture<'static, Result<Option<String>, String>> {
        let text = (file == StoreFile::Cockpit)
            .then(|| self.0.lock().unwrap().clone())
            .flatten();
        Box::pin(async move { Ok(text) })
    }

    fn write(&self, file: StoreFile, text: String) -> BoxFuture<'static, Result<(), String>> {
        if file == StoreFile::Cockpit {
            *self.0.lock().unwrap() = Some(text);
        }
        Box::pin(async { Ok(()) })
    }
}

fn setup(file: Arc<CockpitFile>) -> Setup {
    let mut setup = Setup {
        width: 120,
        height: 30,
        ..Setup::default()
    };
    setup.services.stores = Some(file);
    setup
}

/// ← 之后再打一个字：这一帧画出来时 ← 若打开了任务列表，页已经在（字进了它的筛选）。
async fn left_does_nothing(ui: &mut super::support::Ui) {
    ui.key(KeyCode::Left);
    ui.type_text("z");
    let screen = ui.wait_for(" › z").await;
    assert!(!screen.contains("Agent cockpit"), "{screen}");
    ui.key(KeyCode::Backspace);
}

/// 文件里记着关：启动后槽里没有提示，← 不打开；`/cockpit` 照旧打开。
#[tokio::test(flavor = "multi_thread")]
async fn a_remembered_off_keeps_left_from_opening_the_task_list() {
    let file = Arc::new(CockpitFile(Mutex::new(Some(
        "{\"byFolder\":false,\"leftArrow\":false}\n".to_owned(),
    ))));
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup(file),
        |mut ui| async move {
            let screen = ui.wait_for("? keys").await;
            assert!(!screen.contains("agent cockpit"), "{screen}");
            // 等过连发与打开之后的时限（tui-cockpit.md §3.5），免得手势替它挡掉。
            tokio::time::sleep(Duration::from_millis(1_100)).await;
            left_does_nothing(&mut ui).await;
            ui.submit("/cockpit");
            ui.wait_for("Agent cockpit").await;
        },
    )
    .await;
}

/// 设置 › General 里关掉：当场生效，`cockpit.json` 两样一起写；再打开又回来。
#[tokio::test(flavor = "multi_thread")]
async fn the_settings_switch_takes_effect_at_once_and_is_saved() {
    let file = Arc::new(CockpitFile::default());
    let seen = file.clone();
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup(file),
        |mut ui| async move {
            ui.wait_for("← agent cockpit  ? keys").await;
            ui.submit("/settings");
            // 搜索里 ⏎ 先跳到那一行（General 段，选中它），再 ⏎ 切换。
            ui.type_text("task list");
            ui.wait_for("← opens the task list").await;
            ui.key(KeyCode::Enter);
            ui.wait_for("Settings  ›  General").await;
            ui.key(KeyCode::Enter);
            // 改的当时不说成功（tui-kit.md §4.5「关上时说改了什么」）：行的值换成 off。
            ui.wait("the switch to turn off", |s| {
                s.lines()
                    .any(|l| l.contains("← opens the task list") && l.trim_end().ends_with("off"))
            })
            .await;
            ui.key(KeyCode::Esc);
            ui.key(KeyCode::Esc);
            // 关上时说改了什么。
            let screen = ui.wait_for("Disabled ← opens the task list").await;
            assert!(!screen.contains("agent cockpit"), "{screen}");
            assert!(!screen.contains("Saved."), "{screen}");
            assert_eq!(
                seen.0.lock().unwrap().as_deref(),
                Some("{\"byFolder\":false,\"leftArrow\":false}\n")
            );
            tokio::time::sleep(Duration::from_millis(1_100)).await;
            left_does_nothing(&mut ui).await;
            // 再打开：提示回来，← 打开任务列表。
            ui.submit("/settings");
            // 搜索里 ⏎ 先跳到那一行（General 段，选中它），再 ⏎ 切换。
            ui.type_text("task list");
            ui.wait_for("← opens the task list").await;
            ui.key(KeyCode::Enter);
            ui.wait_for("Settings  ›  General").await;
            ui.key(KeyCode::Enter);
            ui.wait("the switch to turn on", |s| {
                s.lines()
                    .any(|l| l.contains("← opens the task list") && l.trim_end().ends_with("on"))
            })
            .await;
            ui.key(KeyCode::Esc);
            ui.key(KeyCode::Esc);
            ui.wait_for("Enabled ← opens the task list").await;
            ui.wait_for("← agent cockpit  ? keys").await;
            // 上面退格删空过草稿：等过“刚删空要按两下”的 2 s（tui-cockpit.md §3.5）。
            tokio::time::sleep(Duration::from_millis(2_100)).await;
            ui.key(KeyCode::Left);
            ui.wait_for("Agent cockpit").await;
            assert_eq!(
                seen.0.lock().unwrap().as_deref(),
                Some("{\"byFolder\":false,\"leftArrow\":true}\n")
            );
        },
    )
    .await;
}

/// 写不成时（tui-kit.md §4.5「偏好写不成时说一句」）：开关照样当场生效，页的输入框上方说没能保存、只在这次运行有效。
/// 以前任务列表的两样写失败时什么都不说。
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_write_says_it_holds_for_this_run_only() {
    let mut setup = Setup {
        width: 140,
        height: 30,
        ..Setup::default()
    };
    setup.services.stores = Some(Arc::new(FullDisk));
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.wait_for("? keys").await;
            ui.submit("/settings");
            ui.type_text("task list");
            ui.wait_for("← opens the task list").await;
            ui.key(KeyCode::Enter);
            ui.wait_for("Settings  ›  General").await;
            ui.key(KeyCode::Enter);
            ui.wait_for(
                "Couldn't save the ← opens the task list setting, so it holds for this run only: disk full",
            )
            .await;
        },
    )
    .await;
}
