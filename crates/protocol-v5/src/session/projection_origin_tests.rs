//! 后台结果开的回合的回合头带 `originMeta`（protocol-v5.md §47.2，19 §47 R-NOTICE-ORIGIN）：哪项后台工作的结果开的这一轮；活会话
//! （`TurnStarted.works`）与冷读（回合记录的 `works`）同一个函数、同一个值。

use rust_cli_kernel_api::background::NoticedWork;
use rust_cli_kernel_api::ids::{MessageId, TurnId};
use rust_cli_kernel_api::manage::BackgroundKind;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::session_api::{KernelEvent, TurnOrigin};
use rust_cli_kernel_api::store::TurnRecord;
use serde_json::{Value, json};

use super::tests::{live, origin, view};
use super::*;

const TURN: &str = "turn_n";

fn work(id: &str, kind: BackgroundKind, description: &str) -> NoticedWork {
    NoticedWork {
        id: id.to_owned(),
        kind,
        description: description.to_owned(),
    }
}

fn started(origin: TurnOrigin, works: Vec<NoticedWork>) -> KernelEvent {
    KernelEvent::TurnStarted {
        turn: TurnId::new(TURN),
        command: None,
        origin,
        started_at: 5,
        works,
    }
}

fn origin_meta(works: Vec<NoticedWork>) -> Value {
    let (_, publisher) = live(vec![started(TurnOrigin::Notice, works)]);
    json!(publisher.entity(rows::ROWS, TURN).unwrap())["originMeta"].clone()
}

/// 修 bug（19 §47 R-NOTICE-ORIGIN）：以前回合头不写 `originMeta`，界面认不出这是哪项后台工作的结果，结果轮没有结构化的标题、被算进轮次导航。
/// 照 v4：一项是它自己的来源、ID 与说明；冷读按回合记录同值。
#[test]
fn a_background_result_turn_names_its_work_live_and_cold() {
    let works = vec![work("b1", BackgroundKind::Shell, "Print background marker")];
    let (_, publisher) = live(vec![started(TurnOrigin::Notice, works.clone())]);
    let header = json!(publisher.entity(rows::ROWS, TURN).unwrap());
    assert_eq!(
        header["originMeta"],
        json!({"backgroundSource": "bash", "workId": "b1", "title": "Print background marker"})
    );
    // 冷读：通知是一条 meta user 消息（不成行），回合记录带着开它的后台工作
    let notice = Message::User(UserMessage {
        id: MessageId::new("msg_n"),
        content: vec![ContentBlock::Text {
            text: "<task-notification>".to_owned(),
        }],
        origin: UserOrigin::Meta {
            source: "task-notification".to_owned(),
        },
    });
    let record = TurnRecord {
        turn: TurnId::new(TURN),
        first: MessageId::new("msg_n"),
        origin: TurnOrigin::Notice,
        started_at: 5,
        command: None,
        ended: None,
        fell_back: None,
        works,
        compactions: Vec::new(),
    };
    let (_, content) = Projection::build(&view(vec![notice], vec![record]), origin(false));
    let mut cold = Publisher::new("t", "e");
    cold.adopt(content);
    assert_eq!(
        json!(cold.entity(rows::ROWS, TURN).unwrap())["originMeta"],
        header["originMeta"]
    );
}

/// 一轮合了几条通知（照 v4 的合并）：来源与 ID 取第一项，标题是前 3 项的说明用「 · 」接起，多出的写「+N」；子代理是 `subagent`、monitor 归 `bash`
/// （同 `works` 集合）；说明为空时用 ID。
#[test]
fn several_works_share_one_title_like_v4() {
    assert_eq!(
        origin_meta(vec![
            work("agent_1", BackgroundKind::Agent, "Check background"),
            work("b1", BackgroundKind::Shell, "build"),
            work("m1", BackgroundKind::Monitor, ""),
            work("b2", BackgroundKind::Shell, "lint"),
        ]),
        json!({"backgroundSource": "subagent", "workId": "agent_1", "title": "Check background · build · m1 · +1"})
    );
    assert_eq!(
        origin_meta(vec![work("m1", BackgroundKind::Monitor, "  ")]),
        json!({"backgroundSource": "bash", "workId": "m1", "title": "m1"})
    );
}

/// 别的回合没有 `originMeta`：用户输入开的、只有同伴消息或 monitor 事件开的（没有一项后台工作）。
#[test]
fn other_turns_have_no_origin_meta() {
    assert_eq!(origin_meta(Vec::new()), Value::Null);
    let (_, publisher) = live(vec![started(TurnOrigin::Input, Vec::new())]);
    assert!(
        publisher
            .entity(rows::ROWS, TURN)
            .unwrap()
            .get("originMeta")
            .is_none()
    );
}
