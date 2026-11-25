//! 冷读只读最后一段、往前补、激活、保留与下沿之前的计划（protocol-v5.md §73）：同 `release_tests` 的真实 actor 与链路，内核按 `MemoryStore` 的
//! 范围读组冷视图（`Shelf::tail`）。

use super::*;

/// 一行去掉号与版本（§73.1：只读了后缀时号不同；版本是挂进来时的 seq）之后的内容。
fn content_of(row: &Value) -> Value {
    let mut row = row.clone();
    if let Some(fields) = row.as_object_mut() {
        fields.remove(rows::CREATED_AT_SEQ);
        fields.remove("v");
    }
    row
}

/// 这些帧里最后一个 `rowsWindow`（快照的 state 或增量的 state.set）。
fn last_window(frames: &[Value], sub: &str) -> Option<Value> {
    frames
        .iter()
        .filter(|frame| frame["params"]["sub"] == sub)
        .filter_map(|frame| {
            let body = &frame["params"]["body"];
            body["snapshot"]["state"]["rowsWindow"]
                .as_object()
                .map(|window| Value::Object(window.clone()))
                .or_else(|| {
                    body["deltas"].as_array()?.iter().rev().find_map(|delta| {
                        delta["patch"]["rowsWindow"]
                            .as_object()
                            .map(|window| Value::Object(window.clone()))
                    })
                })
        })
        .next_back()
}

/// 冷读只读最后一段的长会话：订阅时一次范围读，不整段读；快照 `earlier: true`、号从 2^32 起。
fn tail_shelf(long: SessionView, store: Arc<MemoryStore>) -> Shelf {
    Shelf {
        store,
        long: Some(long),
        tail: true,
        ..Shelf::default()
    }
}

/// 冷读只读最后一段，往前逐页补到头（protocol-v5.md §73.2、§73.3）：快照与每一页的行与整段建的逐项相同（号与版本除外），号严格递增、一行一个；
/// 只有订阅那一次读了最后一段，之后每补一页读一段，没有整段读；补到头之后 `rowsWindow` 的 `earlier` 变假、`totalCount` 是全部行数。
/// 旧代码上第一个断言就失败（冷读整段读，快照的 `earlier` 不存在、号从 0 起）。
#[tokio::test]
async fn a_cold_tail_pages_back_to_the_whole_projection() {
    pages_back_to_the_whole(long_view(TURNS, 100)).await;
}

/// 没有回合记录的历史（Node 写的，protocol-v5.md §75.3）同样：段首补到输入，每段建出的合成回合（回合头、行的回合 id）与整段建的相同。
/// 旧代码上段首落在回合中间（段里的回合头少一个、行的回合 id 不同）。
#[tokio::test]
async fn a_cold_tail_of_unrecorded_history_pages_back_to_the_whole_projection() {
    let long = SessionView {
        turns: Vec::new(),
        ..long_view(TURNS, 100)
    };
    pages_back_to_the_whole(long).await;
}

/// 长会话的回合数与往前翻一页的行数。
const TURNS: usize = 20;
const PAGE: usize = 50;

async fn pages_back_to_the_whole(long: SessionView) {
    let (_, content) = Projection::build(&long, kit::origin(false));
    let mut whole = kit::publisher();
    whole.adopt(content);
    let expected: Vec<Value> = whole
        .page(rows::ROWS, rows::CREATED_AT_SEQ, None, usize::MAX)
        .unwrap()
        .items;
    let store = seeded(&long).await;
    let mut link = Link::new(tail_shelf(long, store)).await;
    link.subscribe("s", &sid(0)).await;
    let frame = link.snapshot("s").await;
    let window = &frame["body"]["snapshot"]["state"]["rowsWindow"];
    assert_eq!(window["earlier"], json!(true), "{window}");
    let mut seen: Vec<Value> = frame["body"]["snapshot"]["collections"]["rows"]
        .as_array()
        .unwrap()
        .clone();
    assert!(seen.iter().all(|row| {
        row[rows::CREATED_AT_SEQ].as_u64().unwrap() >= crate::consts::ROW_NUMBER_ANCHOR
    }));
    assert!(window["totalCount"].as_u64().unwrap() < expected.len() as u64);
    loop {
        let reply = link
            .request(
                "query",
                json!({"type": "rows.range",
                       "target": {"hostId": "local", "workspaceKey": ROOT, "sessionId": sid(0)},
                       "params": {"beforeId": seen[0]["id"], "limit": PAGE}}),
            )
            .await;
        let data = &reply["result"]["data"];
        let mut page = data["rows"]
            .as_array()
            .unwrap_or_else(|| panic!("{reply}"))
            .clone();
        assert!(
            !page.is_empty() || data["hasMore"] != json!(true),
            "{reply}"
        );
        page.extend(seen);
        seen = page;
        if data["hasMore"] != json!(true) {
            break;
        }
    }
    let ids = |rows: &[Value]| -> Vec<String> {
        rows.iter()
            .map(|row| row["id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(ids(&seen), ids(&expected));
    for (got, want) in seen.iter().zip(&expected) {
        assert_eq!(content_of(got), content_of(want));
    }
    let numbers: Vec<u64> = seen
        .iter()
        .map(|row| row[rows::CREATED_AT_SEQ].as_u64().unwrap())
        .collect();
    assert!(
        numbers.windows(2).all(|pair| pair[0] < pair[1]),
        "{numbers:?}"
    );
    assert_eq!(link.snapshots(), 0, "冷读不应整段读");
    // 每补一页读一段（经存储的查询端口）
    assert!(link.slices.1.load(Ordering::SeqCst) > 0);
    // 补到头之后发过一批 rowsWindow：earlier 变假、已知的行是全部
    let total = expected.len();
    let window = link
        .window_until("s", |window| window["earlier"] == json!(false))
        .await;
    assert_eq!(window["totalCount"], json!(total), "{window}");
}

impl Link {
    /// 收帧直到这个 sub 最后的 `rowsWindow` 合 `done`（往前补的那一批在回复之后才写出）。
    async fn window_until(&mut self, sub: &str, done: impl Fn(&Value) -> bool) -> Value {
        loop {
            if let Some(window) = last_window(&self.frames, sub).filter(|window| done(window)) {
                return window;
            }
            self.pump().await;
        }
    }
}

/// 第一个会话里 `before` 之前的一页（至多 40 行）。
async fn rows_before(link: &mut Link, before: Value) -> Value {
    let request = json!({"type": "rows.range",
           "target": {"hostId": "local", "workspaceKey": ROOT, "sessionId": sid(0)},
           "params": {"beforeId": before, "limit": 40}});
    link.request("query", request).await["result"]["data"]["rows"].clone()
}

/// 冷 → 热（§73.0 第 1 条）：冷读只读最后一段、往前补过一页之后，命令打开了会话：由活视图建的那一份从原来的下沿与号建，不发新快照；
/// 再翻同一页，行与号都与激活之前相同。旧代码上激活时整段建、内容不同，会发一份同 epoch 的新快照。
#[tokio::test]
async fn activating_a_cold_tail_keeps_its_rows_and_numbers() {
    let long = long_view(20, 100);
    let store = seeded(&long).await;
    let mut link = Link::new(tail_shelf(long, store)).await;
    link.subscribe("s", &sid(0)).await;
    let frame = link.snapshot("s").await;
    // 从已知最早一行往前：要往前补一段
    let first = frame["body"]["snapshot"]["state"]["rowsWindow"]["firstId"].clone();
    let before = rows_before(&mut link, first.clone()).await;
    assert!(before.as_array().is_some_and(|rows| !rows.is_empty()));
    let ack = link
        .request(
            "command",
            command(
                ACTIVATE,
                "queue.setAutoDrain",
                &sid(0),
                json!({"autoDrain": true}),
            ),
        )
        .await;
    assert_eq!(ack["result"]["kind"], "accepted", "{ack}");
    // 命令打开了会话（`control.runtime` 变成 ready）：这时由活视图建过投影了
    while !link.frames.iter().any(|frame| {
        frame["params"]["sub"] == "s"
            && frame["params"]["body"]["deltas"]
                .as_array()
                .is_some_and(|deltas| {
                    deltas
                        .iter()
                        .any(|delta| delta["patch"]["control"]["runtime"] == "ready")
                })
    }) {
        link.pump().await;
    }
    // 激活之后的帧里没有快照（只有变了的 state）
    let after = rows_before(&mut link, first).await;
    assert_eq!(after, before);
    assert!(
        !link.frames.iter().any(|frame| frame["params"]["sub"] == "s"
            && frame["params"]["body"]["snapshot"].is_object()),
        "激活不应发新快照"
    );
}

/// 两个窗口同时往前翻同一页（§73.8 第 1 行）：两个补段都读了，只挂进去一次；两个回复的行相同，`rowsWindow` 的已知行数只加一段。
#[tokio::test]
async fn two_concurrent_pages_extend_once() {
    let long = long_view(20, 100);
    let store = seeded(&long).await;
    let mut link = Link::new(tail_shelf(long, store)).await;
    link.subscribe("s", &sid(0)).await;
    let frame = link.snapshot("s").await;
    let window = &frame["body"]["snapshot"]["state"]["rowsWindow"];
    let total = window["totalCount"].as_u64().unwrap();
    // 从已知最早一行往前：Publisher 里之前没有行，要往前补
    let first = window["firstId"].clone();
    let params = json!({"type": "rows.range",
           "target": {"hostId": "local", "workspaceKey": ROOT, "sessionId": sid(0)},
           "params": {"beforeId": first, "limit": 30}});
    let one = link.send("query", params.clone()).await;
    let two = link.send("query", params).await;
    let one = link.reply(one).await;
    let two = link.reply(two).await;
    let rows_of = |reply: &Value| reply["result"]["data"]["rows"].clone();
    assert_eq!(rows_of(&one), rows_of(&two), "{one} {two}");
    let added = rows_of(&one).as_array().unwrap().len() as u64;
    let window = link
        .window_until("s", |window| window["totalCount"].as_u64() > Some(total))
        .await;
    assert!(window["totalCount"].as_u64().unwrap() >= total + added);
    assert!(
        window["totalCount"].as_u64().unwrap() < total + 2 * added,
        "{window}"
    );
}

/// 下沿之前的计划（§73.4）：冷读只读最后一段，第一个回合里的 ExitPlanMode 不在投影里；`session.plans` 读库补上它（排在最后：最新的在前）。
/// 旧代码只按 Publisher 答，没有它。
#[tokio::test]
async fn plans_before_the_floor_come_from_the_store() {
    let mut long = long_view(20, 100);
    // 第一个回合的第一次调用换成 ExitPlanMode（带计划文字）
    for message in &mut long.messages {
        if let Message::Assistant(assistant) = message
            && assistant.id.as_str() == "msg_a0_0"
            && let Some(ContentBlock::ToolUse { name, input, .. }) = assistant.content.first_mut()
        {
            *name = rust_cli_kernel_api::tool_names::EXIT_PLAN_MODE.to_owned();
            *input = json!({"plan": "the early plan"});
        }
    }
    let store = seeded(&long).await;
    let mut link = Link::new(tail_shelf(long, store)).await;
    link.subscribe("s", &sid(0)).await;
    let frame = link.snapshot("s").await;
    assert_eq!(
        frame["body"]["snapshot"]["state"]["rowsWindow"]["earlier"],
        json!(true)
    );
    let reply = link
        .request(
            "query",
            json!({"type": "session.plans",
                   "target": {"hostId": "local", "workspaceKey": ROOT, "sessionId": sid(0)},
                   "params": {}}),
        )
        .await;
    let plans = reply["result"]["data"]["plans"]
        .as_array()
        .unwrap_or_else(|| panic!("{reply}"));
    let ids: Vec<&str> = plans
        .iter()
        .filter_map(|plan| plan["id"].as_str())
        .collect();
    assert_eq!(ids, ["toolu_0_0"], "{reply}");
}

/// 保留（§70）带着补过的段：冷读只读最后一段、往前补过一段之后退订（进保留），重订复用（epoch 不变、`rowsWindow` 里有补过的那段），
/// 同一页从 Publisher 答、不再读库。
#[tokio::test]
async fn a_kept_tail_keeps_its_extension() {
    let long = long_view(20, 100);
    let store = seeded(&long).await;
    let mut link = Link::new(tail_shelf(long, store)).await;
    link.subscribe("s", &sid(0)).await;
    let frame = link.snapshot("s").await;
    let epoch = frame["body"]["snapshot"]["cursor"]["epoch"].clone();
    let first = frame["body"]["snapshot"]["state"]["rowsWindow"]["firstId"].clone();
    let page = rows_before(&mut link, first.clone()).await;
    let window = link
        .window_until("s", |window| window["firstId"] != first)
        .await;
    let ranges = link.slices.1.load(Ordering::SeqCst);
    link.unsubscribe("s").await;
    let subscribed = link.subscribe("s2", &sid(0)).await;
    assert!(subscribed["result"]["pending"].is_null(), "{subscribed}");
    let again = link.snapshot("s2").await;
    assert_eq!(again["body"]["snapshot"]["cursor"]["epoch"], epoch);
    assert_eq!(again["body"]["snapshot"]["state"]["rowsWindow"], window);
    assert_eq!(rows_before(&mut link, first).await, page);
    assert_eq!(
        link.slices.1.load(Ordering::SeqCst),
        ranges,
        "保留的投影不应再读库"
    );
}
