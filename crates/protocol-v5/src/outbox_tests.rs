use serde_json::json;

use super::*;
use crate::publisher::Fields;

fn topics(phase: &str) -> BTreeMap<String, Publisher> {
    let mut p = Publisher::new("agent", "e");
    p.set_state([("phase".to_owned(), json!(phase))].into());
    [("agent".to_owned(), p)].into()
}

fn set(topics: &mut BTreeMap<String, Publisher>, phase: &str) {
    topics
        .get_mut("agent")
        .unwrap()
        .set_state([("phase".to_owned(), json!(phase))].into());
}

fn kinds(out: &[Outgoing]) -> Vec<String> {
    out.iter()
        .map(|o| match o {
            Outgoing::Notify { method, params } => {
                format!(
                    "{method}:{}",
                    params["body"]["kind"].as_str().unwrap_or("-")
                )
            }
            Outgoing::Frame { .. } => {
                format!("frame:{}", frame(o)["body"]["kind"].as_str().unwrap_or("-"))
            }
            Outgoing::Respond { id, .. } => format!("reply:{id}"),
        })
        .collect()
}

/// 一帧的 params（出站队列交出的是编好的文字，§68.1）。
fn frame(out: &Outgoing) -> Value {
    let Outgoing::Frame { params } = out else {
        panic!("not a frame: {out:?}")
    };
    serde_json::from_str(params).unwrap()
}

#[test]
fn subscribed_comes_after_the_first_frame_with_its_cursor() {
    let topics = topics("ready");
    let mut outbox = Outbox::new(4, 1 << 20);
    outbox.subscribe(json!(1), "s", "agent", None);
    let out = outbox.pump(&topics);
    assert_eq!(kinds(&out), ["frame:snapshot", "reply:1"]);
    let Outgoing::Respond {
        result: Ok(result), ..
    } = &out[1]
    else {
        panic!()
    };
    assert_eq!(
        result,
        &json!({"sub": "s", "cursor": {"epoch": "e", "seq": 1}})
    );
}

#[test]
fn a_cursor_that_is_already_current_gets_no_frame() {
    let topics = topics("ready");
    let mut outbox = Outbox::new(4, 1 << 20);
    outbox.subscribe(
        json!(1),
        "s",
        "agent",
        Some(Cursor {
            epoch: "e".into(),
            seq: 1,
        }),
    );
    assert_eq!(kinds(&outbox.pump(&topics)), ["reply:1"]);
}

#[test]
fn without_credit_the_head_blocks_everything_behind_it() {
    let mut topics = topics("starting");
    let mut outbox = Outbox::new(1, 1 << 20);
    outbox.subscribe(json!(1), "s", "agent", None);
    assert_eq!(kinds(&outbox.pump(&topics)), ["frame:snapshot", "reply:1"]);
    // 信用用完：效果帧挡在队头，ACK 排在它后面一起等（G2）
    set(&mut topics, "ready");
    outbox.mark_topic("agent");
    outbox.reply(json!(2), Ok(json!({"kind": "accepted"})));
    assert!(outbox.pump(&topics).is_empty());
    assert!(outbox.blocked());
    // 期间的多次发布折叠成一帧
    set(&mut topics, "stopped");
    outbox.mark_topic("agent");
    outbox.credit("s", 1);
    let out = outbox.pump(&topics);
    assert_eq!(kinds(&out), ["frame:deltas", "reply:2"]);
    let params = frame(&out[0]);
    assert_eq!(
        (params["from"].clone(), params["to"].clone()),
        (json!(1), json!(3))
    );
}

#[test]
fn unsubscribing_the_blocked_head_unblocks_the_queue() {
    let mut topics = topics("starting");
    let mut outbox = Outbox::new(1, 1 << 20);
    outbox.subscribe(json!(1), "s", "agent", None);
    outbox.pump(&topics);
    set(&mut topics, "ready");
    outbox.mark_topic("agent");
    outbox.reply(json!(2), Ok(json!({})));
    assert!(outbox.pump(&topics).is_empty());
    assert!(outbox.unsubscribe("s"));
    assert_eq!(kinds(&outbox.pump(&topics)), ["reply:2"]);
    assert!(!outbox.unsubscribe("s"));
}

#[test]
fn a_frame_over_the_limit_falls_back_to_a_snapshot_then_fails_the_subscription() {
    let mut topics = topics("starting");
    let items = (0..64)
        .map(|n| (format!("item-{n}"), Fields::new()))
        .collect::<Vec<_>>();
    topics.get_mut("agent").unwrap().put("items", items.clone());
    let mut outbox = Outbox::new(8, 1 << 20);
    outbox.subscribe(json!(1), "s", "agent", None);
    outbox.pump(&topics);
    // 订阅拿到之后都删掉：续传帧是 64 条 remove，超限；快照里集合已空，不超限 → 发快照（续传帧从当前内容拼，01 §19.2：
    // 只有删掉的项会让它比快照大）
    for (id, _) in &items {
        let mut batch = crate::publisher::Batch::default();
        batch.remove("items", id);
        topics.get_mut("agent").unwrap().publish(batch);
    }
    outbox.mark_topic("agent");
    let tight = serde_json::to_string(&topics["agent"].frame_for(None, "s", false))
        .unwrap()
        .len()
        + 64;
    outbox.max_message_bytes = tight;
    assert_eq!(kinds(&outbox.pump(&topics)), ["frame:snapshot"]);
    // 快照也超限：结束这个订阅
    set(&mut topics, &"x".repeat(tight));
    outbox.mark_topic("agent");
    let out = outbox.pump(&topics);
    assert_eq!(kinds(&out), ["subscription.failed:-"]);
    let Outgoing::Notify { params, .. } = &out[0] else {
        panic!()
    };
    assert_eq!(params["sub"], json!("s"));
    assert_eq!(params["error"]["code"], json!("link.messageTooLarge"));
    assert!(outbox.subscription("s").is_none());
}

#[test]
fn resubscribing_the_same_sub_keeps_its_credit_and_its_place() {
    let mut topics = topics("starting");
    let mut outbox = Outbox::new(2, 1 << 20);
    outbox.subscribe(json!(1), "s", "agent", None);
    assert_eq!(kinds(&outbox.pump(&topics)), ["frame:snapshot", "reply:1"]);
    set(&mut topics, "ready");
    outbox.mark_topic("agent");
    outbox.pump(&topics);
    // 信用用完；重订不重置信用（01 §4.3：重订之前发出的帧还在途，client 收到后照样补，重置再叠加就超出窗口）
    outbox.subscribe(
        json!(2),
        "s",
        "agent",
        Some(Cursor {
            epoch: "other".into(),
            seq: 0,
        }),
    );
    assert!(outbox.pump(&topics).is_empty());
    assert_eq!(outbox.subscription("s").unwrap().credit, 0);
    outbox.credit("s", 1);
    assert_eq!(kinds(&outbox.pump(&topics)), ["frame:snapshot", "reply:2"]);
}

#[test]
fn a_closed_outbox_marks_nothing_new_but_still_sends_what_was_queued() {
    let mut outbox = Outbox::new(1, usize::MAX);
    let mut t = topics("starting");
    outbox.subscribe(json!(1), "a", "agent", None);
    assert_eq!(kinds(&outbox.pump(&t)), ["frame:snapshot", "reply:1"]);
    // 信用用完：这次发布挂在队头
    set(&mut t, "ready");
    outbox.mark_topic("agent");
    outbox.close();
    outbox.reply(json!(2), Ok(json!({})));
    set(&mut t, "stopped");
    outbox.mark_topic("agent");
    assert!(outbox.pump(&t).is_empty());
    outbox.credit("a", 5);
    // 已挂的标记照发（取最新状态），关闭之后的发布不再挂第二个
    assert_eq!(kinds(&outbox.pump(&t)), ["frame:deltas", "reply:2"]);
    assert!(outbox.pump(&t).is_empty());
}

#[test]
fn a_pending_subscription_answers_pending_and_gets_the_snapshot_once_the_topic_exists() {
    let mut topics = BTreeMap::new();
    let mut outbox = Outbox::new(4, 1 << 20);
    outbox.subscribe_pending(json!(1), "s", "agent", None);
    let out = outbox.pump(&topics);
    assert_eq!(kinds(&out), ["reply:1"]);
    let Outgoing::Respond {
        result: Ok(result), ..
    } = &out[0]
    else {
        panic!()
    };
    assert_eq!(
        result,
        &json!({"sub": "s", "pending": "source_unavailable"})
    );
    // 登记之后挂标记：首帧是快照
    topics.extend(self::topics("ready"));
    outbox.mark_topic("agent");
    assert_eq!(kinds(&outbox.pump(&topics)), ["frame:snapshot"]);
}

#[test]
fn a_failed_source_ends_the_waiting_subscriptions_in_queue_order() {
    let topics = BTreeMap::new();
    let mut outbox = Outbox::new(4, 1 << 20);
    outbox.subscribe_pending(json!(1), "s", "agent", None);
    outbox.subscribe_pending(json!(2), "t", "other", None);
    let error = protocol_error(codes::PRECONDITION_STORE_UNAVAILABLE, "no store");
    outbox.fail_topic("agent", &error);
    let out = outbox.pump(&topics);
    assert_eq!(kinds(&out), ["reply:1", "reply:2", "subscription.failed:-"]);
    let Outgoing::Notify { params, .. } = &out[2] else {
        panic!()
    };
    assert_eq!(params["sub"], json!("s"));
    assert_eq!(
        params["error"]["code"],
        json!("precondition.storeUnavailable")
    );
    assert!(outbox.subscription("s").is_none());
    assert!(outbox.subscription("t").is_some());
    // shutdown 之后不再排通知
    outbox.close();
    outbox.fail_topic("other", &error);
    assert!(outbox.pump(&topics).is_empty());
}

/// 已发出的下沿（protocol-v5.md §68.0 第 5 条）：这个 topic 的在订订阅里发到的最小位置；没有在订的订阅时是当前位置。
/// 换骨架只换不晚于它的项。
#[test]
fn the_sent_floor_is_the_least_position_the_topic_subscriptions_have_reached() {
    let mut topics = topics("starting");
    let current = |topics: &BTreeMap<String, Publisher>| topics["agent"].cursor();
    let mut outbox = Outbox::new(1, 1 << 20);
    assert_eq!(outbox.sent_floor("agent", &current(&topics)), 1);
    outbox.subscribe(json!(1), "s", "agent", None);
    outbox.subscribe(json!(2), "t", "agent", None);
    outbox.pump(&topics);
    // 两个订阅都只有一个信用、都用在快照上；只给 s 补信用，t 停在快照的位置
    set(&mut topics, "ready");
    outbox.mark_topic("agent");
    outbox.credit("s", 1);
    outbox.pump(&topics);
    assert_eq!(current(&topics).seq, 2);
    assert_eq!(outbox.sent_floor("agent", &current(&topics)), 1);
    outbox.credit("t", 1);
    outbox.pump(&topics);
    assert_eq!(outbox.sent_floor("agent", &current(&topics)), 2);
    // 别的 topic 不算
    assert_eq!(outbox.sent_floor("other", &current(&topics)), 2);
}
