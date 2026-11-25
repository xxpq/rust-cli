//! 访问钥匙串之前的提示（docs/specs/rust-cli/tui.md §7.10，platform.md §13.12）：入口发一个信号，footer 提示一句，语言照
//! 界面的规则。
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::testing::text_response;
use rust_cli_tui::{KeychainNotices, Services, UiLocale};
use tokio::sync::mpsc;

use super::support::{GatedModel, Setup, with_ui};

/// 测试拿着发送端，界面取信号流。
struct Notices(Mutex<Option<mpsc::UnboundedReceiver<()>>>);

impl KeychainNotices for Notices {
    fn notices(&self) -> BoxStream<'static, ()> {
        let receiver = self.0.lock().unwrap().take().unwrap();
        futures_util::stream::unfold(receiver, |mut receiver| async move {
            receiver.recv().await.map(|()| ((), receiver))
        })
        .chain(futures_util::stream::pending())
        .boxed()
    }
}

fn setup(locale: UiLocale) -> (Setup, mpsc::UnboundedSender<()>) {
    let (sender, receiver) = mpsc::unbounded_channel();
    let setup = Setup {
        ui: rust_cli_tui::UiConfig {
            locale: Some(locale),
            ..rust_cli_tui::UiConfig::default()
        },
        services: Services {
            keychain: Some(Arc::new(Notices(Mutex::new(Some(receiver))))),
            ..super::stub_services::services()
        },
        ..Setup::default()
    };
    (setup, sender)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_keychain_notice_shows_in_the_footer() {
    for (locale, text) in [
        (
            UiLocale::EnUs,
            "Reading the login key from the keychain; macOS may ask.",
        ),
        (
            UiLocale::ZhCn,
            "要读取钥匙串里的登录密钥，系统可能会弹框询问。",
        ),
    ] {
        let (setup, sender) = setup(locale);
        let model = GatedModel::scripted(vec![text_response(&["ok"])]);
        with_ui(model, setup, |mut ui| async move {
            sender.send(()).unwrap();
            ui.wait_for(text).await;
        })
        .await;
    }
}
