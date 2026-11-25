//! 问题导航的目录（protocol-v5.md §73.5）：每条真实用户输入一项、同一回合共用回答、没有回合记录的按输入切出回合（§75.3）、文字截断；
//! 按存储从尾往前一页一页给全、`before` 到头为空；不认识的 `beforeId` → notFound。

use std::sync::Arc;

use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{InputEnd, TurnOrigin};
use rust_cli_kernel_api::store::{Fact, FactBatch, SessionMeta, SessionStore, TurnRecord};
use rust_cli_kernel_api::testing::MemoryStore;
use serde_json::Value;

use super::*;
use crate::session::projection::kit::{assistant, ended, turn_record, user, view};

fn text_block(text: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: text.to_owned(),
    }]
}

/// 由视图建出、按号排好的行。
fn built_rows(view: &SessionView) -> Vec<(String, Fields)> {
    let (_, mut content) = Projection::build(view, Origin::default());
    let mut built: Vec<(String, Fields)> = content
        .collections
        .remove(ROWS)
        .unwrap_or_default()
        .into_iter()
        .collect();
    built.sort_by_key(|(_, fields)| fields.get(CREATED_AT_SEQ).and_then(Value::as_u64));
    built
}

/// 一个回合里两条输入（各一项、共用回合的回答）；不属于回合的两条输入各自的回答到下一条输入为止；回答按字符截断。
#[test]
fn every_real_input_is_an_item_and_answers_follow_their_turn() {
    let long = "长".repeat(PROMPT_TEXT_CHARS + 10);
    // 同一回合里的第二条输入（没有回合记录从它开头，归 turn_1）
    let guided = user("msg_g", "also this");
    let mut record = turn_record("turn_1", "msg_u1", TurnOrigin::Input);
    record.ended = ended(InputEnd::Completed, Vec::new());
    let turned = view(
        vec![
            user("msg_u1", &long),
            assistant("msg_a1", text_block("first answer")),
            guided,
            assistant("msg_a2", text_block("second answer")),
        ],
        vec![record],
    );
    let items = prompts(&built_rows(&turned));
    let ids: Vec<&str> = items.iter().map(|item| item.row_id.as_str()).collect();
    assert_eq!(ids, ["msg_u1", "msg_g"]);
    assert_eq!(items[0].text.chars().count(), PROMPT_TEXT_CHARS);
    assert_eq!(items[0].answer, ["first answer", "second answer"]);
    assert_eq!(items[1].answer, items[0].answer);
    assert_eq!(items[0].turn_id, "turn_1");
    // 没有回合记录（Node 写的历史）：按输入切出回合（protocol-v5.md §75.3），回答是这个回合的正文行，与到下一条输入为止取的相同
    let plain = view(
        vec![
            user("msg_x", "one"),
            assistant("msg_y", text_block(&"a".repeat(PROMPT_ANSWER_CHARS + 5))),
            assistant("msg_z", text_block("more")),
            user("msg_w", "two"),
        ],
        Vec::new(),
    );
    let items = prompts(&built_rows(&plain));
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].turn_id, "turn_of_msg_x");
    assert_eq!(items[1].turn_id, "turn_of_msg_w");
    assert_eq!(
        items[0].answer.concat().chars().count(),
        PROMPT_ANSWER_CHARS
    );
    assert!(items[1].answer.is_empty());
}

/// `turns` 个回合（每回合一条输入、一条回答），写进存储。
async fn stored(turns: usize) -> Arc<MemoryStore> {
    let store = Arc::new(MemoryStore::default());
    let sid = SessionId::new("ses_1");
    let meta = SessionMeta {
        id: sid.clone(),
        ..view(Vec::new(), Vec::new()).meta
    };
    let batch = |facts| FactBatch {
        command: None,
        facts,
        turn: None,
    };
    store.create(meta, batch(Vec::new())).await.unwrap();
    let mut facts = Vec::new();
    for turn in 0..turns {
        let prompt = format!("msg_u{turn}");
        facts.push(Fact::MessageAppended {
            message: user(&prompt, &format!("question {turn}")),
        });
        facts.push(
            TurnRecord::opened(
                rust_cli_kernel_api::ids::TurnId::new(format!("turn_{turn}")),
                MessageId::new(&prompt),
                TurnOrigin::Input,
                1,
            )
            .fact(),
        );
        facts.push(Fact::MessageAppended {
            message: assistant(
                &format!("msg_a{turn}"),
                text_block(&format!("answer {turn}")),
            ),
        });
    }
    store.commit(&sid, batch(facts)).await.unwrap();
    store
}

fn defaults() -> (rust_cli_kernel_api::model::ModelSelection, PermissionMode) {
    (
        rust_cli_kernel_api::testing::selection(),
        PermissionMode::Default,
    )
}

/// 从最新起一页一页往前，直到 `before` 为空：每个回合的提问一项、按显示顺序、回答是它那一轮的；页不重叠；不认识的 `beforeId` → notFound。
#[tokio::test]
async fn pages_from_the_tail_cover_every_prompt() {
    const TURNS: usize = PROMPTS_PAGE_RECORDS; // 一页按存储消息算，每回合两条：至少两页
    let store = stored(TURNS).await;
    let mut before: Option<String> = None;
    let mut pages: Vec<Vec<(String, Vec<String>)>> = Vec::new();
    loop {
        let data = page(store.clone(), "ses_1", before.clone(), defaults())
            .await
            .unwrap();
        let result: QuerySessionPromptsResult = serde_json::from_value(data).unwrap();
        pages.push(
            result
                .items
                .into_iter()
                .map(|item| (item.row_id, item.answer))
                .collect(),
        );
        match result.before {
            Some(next) => before = Some(next),
            None => break,
        }
    }
    assert!(pages.len() >= 2, "{}", pages.len());
    let all: Vec<(String, Vec<String>)> = pages.into_iter().rev().flatten().collect();
    let expected: Vec<(String, Vec<String>)> = (0..TURNS)
        .map(|turn| (format!("msg_u{turn}"), vec![format!("answer {turn}")]))
        .collect();
    assert_eq!(all, expected);
    let missing = page(store, "ses_1", Some("msg_gone".to_owned()), defaults()).await;
    assert_eq!(missing.unwrap_err().code, codes::PRECONDITION_NOT_FOUND);
}
