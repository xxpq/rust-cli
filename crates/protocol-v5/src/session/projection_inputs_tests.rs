//! 行上的高速卡事实（protocol-v5.md §46，23 §13）：输入行的 `highspeed` 是注记拼上这一轮的退回事实，活会话与冷读同一个函数、同一个值；
//! 上报之后重写这一行；回合失败的 `lastError.attribution` 带这一轮的卡号或 null。

use rust_cli_kernel_api::input_extras::InputRecord;
use rust_cli_kernel_api::model::ModelError;
use rust_cli_kernel_api::turn_execution::FellBack;

use super::*;

const EXPIRED: &str = "highspeed_card_expired";
const FALLBACK_AT: u64 = 150;

fn meta() -> Value {
    json!({"schemaVersion": 1, "cardId": "hsc_1", "taskId": "ses_1", "provider": "builtin:bigmodel-coding-plan",
        "model": rust_cli_base::models::GLM_5_3, "issuedAt": 10, "expiresAt": 9_000})
}

fn noted(note: Value) -> InputRecord {
    InputRecord {
        note: Some(note),
        ..InputRecord::from_entry("msg_u", &json!({})).unwrap()
    }
}

fn fell_back() -> FellBack {
    FellBack {
        reason: EXPIRED.to_owned(),
        at: FALLBACK_AT,
    }
}

/// `meta` 拼上这一轮的退回事实：界面读的就是这一份（v4 `highspeedMessageMetaSchema` 的键）。
fn with_fallback(mut note: Value) -> Value {
    note["fallbackAt"] = json!(FALLBACK_AT);
    note["fallbackReason"] = json!(EXPIRED);
    note
}

fn row(publisher: &Publisher, id: &str) -> Value {
    rows_of(publisher)
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap()
        .clone()
}

/// 活会话：输入记录先于用户行，退回在 `at` 处。
fn events(at: usize) -> (Vec<Message>, Vec<KernelEvent>) {
    let (messages, mut events) = conversation();
    events.insert(
        1,
        KernelEvent::InputRecorded {
            record: noted(meta()),
        },
    );
    events.insert(
        at,
        KernelEvent::ExecutionFellBack {
            turn: TurnId::new(TURN),
            fell_back: fell_back(),
        },
    );
    (messages, events)
}

fn cold(messages: Vec<Message>, record: TurnRecord, inputs: Vec<InputRecord>) -> Publisher {
    let cold_view = SessionView {
        inputs,
        tool_times: vec![tool_time()],
        ..view(messages, vec![record])
    };
    let (_, content) = Projection::build(&cold_view, origin(false));
    let mut publisher = Publisher::new("t", "e").with_window(
        rows::ROWS,
        rows::CREATED_AT_SEQ,
        SESSION_SNAPSHOT_ROWS,
        usize::MAX,
    );
    publisher.adopt(content);
    publisher
}

/// 加速的一轮退回了：活会话（输入记录、退回事件）与冷读（视图的 `inputs`、回合记录的 `fell_back`）给出同一组行；只有输入行带 `highspeed`。
/// 以前 rust 丢掉 `highspeedMeta`，行上没有卡，界面不画紫色的「已工作」、不提示退回。
#[test]
fn the_input_row_carries_the_card_and_its_fallback_live_and_cold() {
    let (messages, events) = events(3);
    let (_, live_publisher) = live(events);
    let recorded = TurnRecord {
        fell_back: Some(fell_back()),
        ..record()
    };
    let cold_publisher = cold(messages, recorded, vec![noted(meta())]);
    let rows = contents(&rows_of(&live_publisher));
    assert_eq!(rows, contents(&rows_of(&cold_publisher)));
    assert_eq!(
        row(&live_publisher, "msg_u")["highspeed"],
        with_fallback(meta())
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.get("highspeed").is_some())
            .count(),
        1
    );
}

/// 退回事件早于用户行（不会发生，但两种先后都对）：建用户行时读回合的退回事实，同一行。
#[test]
fn a_fallback_seen_before_the_input_row_gives_the_same_row() {
    let (_, early) = live(events(2).1);
    let (_, late) = live(events(3).1);
    assert_eq!(contents(&rows_of(&early)), contents(&rows_of(&late)));
}

/// 回合中途建的投影（视图的回合记录还没有结局、只在内存里有退回）：同样带退回事实。
#[test]
fn a_projection_built_mid_turn_keeps_the_fallback() {
    let running = TurnRecord {
        ended: None,
        fell_back: Some(fell_back()),
        ..record()
    };
    let publisher = cold(vec![user("msg_u", "hi")], running, vec![noted(meta())]);
    assert_eq!(row(&publisher, "msg_u")["highspeed"], with_fallback(meta()));
}

/// 上报之后（内核再发这条输入的记录）：输入行换上合并后的注记，退回事实还在；冷读同值。
#[test]
fn a_report_rewrites_the_card_and_keeps_the_fallback() {
    let mut reported = meta();
    reported["savedDurationMs"] = json!(900);
    reported["highspeedTps"] = json!(160);
    let (messages, mut events) = events(3);
    events.push(KernelEvent::InputRecorded {
        record: noted(reported.clone()),
    });
    let (_, live_publisher) = live(events);
    assert_eq!(
        row(&live_publisher, "msg_u")["highspeed"],
        with_fallback(reported.clone())
    );
    let recorded = TurnRecord {
        fell_back: Some(fell_back()),
        ..record()
    };
    let cold_publisher = cold(messages, recorded, vec![noted(reported)]);
    assert_eq!(
        contents(&rows_of(&live_publisher)),
        contents(&rows_of(&cold_publisher))
    );
}

fn failed(events: &mut [KernelEvent]) {
    let last = events.len() - 1;
    events[last] = KernelEvent::TurnFinished {
        turn: TurnId::new(TURN),
        outcome: TurnOutcome::Failed {
            message: "server error".to_owned(),
            cause: Some(ModelError::Server {
                status: 500,
                message: "x".to_owned(),
            }),
        },
        usage: None,
        time: super::kit::ENDED,
        files: Vec::new(),
    };
}

/// 回合以失败结束：`lastError.attribution` 带这一轮的卡号；没有卡的一轮写 null（界面据此不按错误码猜，23 §13.2 第 5 条）。
#[test]
fn a_failed_turn_names_its_card_or_null() {
    let (_, mut with_card) = events(3);
    failed(&mut with_card);
    let (_, publisher) = live(with_card);
    assert_eq!(
        snapshot(&publisher)["state"]["control"]["lastError"]["attribution"],
        json!({"highspeedCardId": "hsc_1"})
    );
    let (_, mut plain) = conversation();
    failed(&mut plain);
    let (_, publisher) = live(plain);
    assert_eq!(
        snapshot(&publisher)["state"]["control"]["lastError"]["attribution"],
        json!({"highspeedCardId": null})
    );
}

/// 输入行带这一轮用的配置（§64.2）：输入记录的 `config` 写成行的 `modelSelection` / `mode` / `planEnabled`（App 的模式名），活会话与冷读同一行。
/// 以前行上没有这三项，编辑最后一条时界面载入的是当前的草稿配置。
#[test]
fn the_input_row_carries_the_config_it_ran_with_live_and_cold() {
    let configured = InputRecord {
        config: rust_cli_kernel_api::input_extras::TurnConfig {
            selection: Some(Box::new(rust_cli_kernel_api::model::ModelSelection::new(
                "provider_a",
                "model_a",
            ))),
            mode: Some(rust_cli_kernel_api::permission::PermissionMode::AcceptEdits),
            plan: Some(true),
        },
        ..InputRecord::from_entry("msg_u", &json!({})).unwrap()
    };
    let (messages, mut events) = conversation();
    events.insert(
        1,
        KernelEvent::InputRecorded {
            record: configured.clone(),
        },
    );
    let (_, live_publisher) = live(events);
    let cold_publisher = cold(messages, record(), vec![configured]);
    assert_eq!(
        contents(&rows_of(&live_publisher)),
        contents(&rows_of(&cold_publisher))
    );
    let row = row(&live_publisher, "msg_u");
    assert_eq!(row["mode"], "edit");
    assert_eq!(row["planEnabled"], true);
    assert_eq!(row["modelSelection"]["modelId"], "model_a");
}
