//! 已定前缀的对拍（docs/specs/rust-cli/request-prefix.md §4、§5）：随机生成的会话逐步追加，每一步都比
//! 「上一步定下的前缀 + 从切点接着规范化」与「整段规范化」，逐条相同；表的校验在回退、压缩、换键、没交回时整段重建。

use std::sync::Arc;

use rust_cli_kernel_api::hooks::queued_command;
use rust_cli_kernel_api::ids::{MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, CompactBoundary, CompactTrigger, ContentBlock,
    ImageSource, Message, SystemMessage, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::ApiMessage;
use rust_cli_kernel_api::request_prefix::WirePrefix;
use rust_cli_kernel_api::store::ArtifactStore;
use rust_cli_kernel_api::testing::MemoryArtifacts;
use rust_cli_prompts::models::ToolSearchMode;
use serde_json::json;

use super::*;
use crate::normalize::{NormalizeOptions, normalize, normalize_from};

/// 确定的伪随机数（xorshift）：同一个种子生成同一段会话。
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

fn text(value: &str) -> ContentBlock {
    ContentBlock::Text {
        text: value.to_owned(),
    }
}

fn user(id: String, content: Vec<ContentBlock>, origin: UserOrigin) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content,
        origin,
    })
}

fn attachment(id: String, source: &str, body: &str, data: serde_json::Value) -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new(id),
        source: source.to_owned(),
        text: if body.is_empty() {
            String::new()
        } else {
            rust_cli_prompts::reminder::wrap(body)
        },
        data,
    })
}

/// 一段会话：人类输入、各种来源的 attachment（system 与 user 角色、插话、通知、带图片、空的）、各种形状的回复（文字、
/// thinking、工具调用、重复的 id、空回复）、工具结果（正常、出错、缺的、孤立的）、中断标记。
fn history(seed: u64, length: usize) -> Vec<Message> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut out = Vec::new();
    let mut pending: Vec<ToolUseId> = Vec::new();
    let mut calls = 0;
    let mut last_assistant = MessageId::new("none");
    for index in 0..length {
        let id = format!("m{index}");
        let roll = rng.below(100);
        let message = if !pending.is_empty() && roll < 45 {
            // 工具结果：大多数配对，偶尔缺一个或多一个孤立的。
            let calls_now: Vec<ToolUseId> = std::mem::take(&mut pending);
            let mut content: Vec<ContentBlock> = Vec::new();
            for id in calls_now {
                if rng.chance(8) {
                    continue;
                }
                content.push(ContentBlock::ToolResult {
                    tool_use_id: id,
                    content: match rng.below(3) {
                        0 => Vec::new(),
                        1 => vec![ToolResultContent::Text {
                            text: format!("out {index}"),
                        }],
                        _ => vec![
                            ToolResultContent::Text {
                                text: "a".to_owned(),
                            },
                            ToolResultContent::Text {
                                text: "b".to_owned(),
                            },
                        ],
                    },
                    is_error: rng.chance(10),
                });
            }
            if rng.chance(5) {
                content.push(ContentBlock::ToolResult {
                    tool_use_id: ToolUseId::new(format!("orphan{index}")),
                    content: Vec::new(),
                    is_error: false,
                });
            }
            user(
                id,
                content,
                UserOrigin::ToolResult {
                    assistant: last_assistant.clone(),
                },
            )
        } else if roll < 60 {
            let mut content = Vec::new();
            if rng.chance(15) {
                content.push(ContentBlock::Thinking {
                    thinking: format!("think {index}"),
                    signature: Some("sig".to_owned()),
                });
            }
            if rng.chance(70) {
                content.push(text(&format!("answer {index}")));
            }
            let tools = rng.below(3);
            for _ in 0..tools {
                calls += 1;
                // 偶尔重复一个已经发过的 id（配对兜底按 `seen` 去掉）。
                let tool = if calls > 2 && rng.chance(4) {
                    ToolUseId::new(format!("t{}", calls - 2))
                } else {
                    ToolUseId::new(format!("t{calls}"))
                };
                pending.push(tool.clone());
                content.push(ContentBlock::ToolUse {
                    id: tool,
                    name: ["Read", "Bash", "Agent", "mcp__x__y"][rng.below(4) as usize].to_owned(),
                    input: json!({"n": index}),
                });
            }
            if tools > 0 && rng.chance(20) {
                content.push(text("after the call"));
            }
            last_assistant = MessageId::new(&id);
            Message::Assistant(AssistantMessage {
                id: MessageId::new(id),
                content,
                model: rust_cli_base::models::GLM_5_3.to_owned(),
                stop_reason: None,
                usage: None,
            })
        } else if roll < 72 {
            pending.clear();
            match rng.below(4) {
                0 => user(id, Vec::new(), UserOrigin::Human),
                1 => user(
                    id,
                    vec![text("continue")],
                    UserOrigin::Meta {
                        source: "nudge".to_owned(),
                    },
                ),
                _ => user(id, vec![text(&format!("ask {index}"))], UserOrigin::Human),
            }
        } else if roll < 95 {
            match rng.below(7) {
                0 => attachment(id, "date_change", &format!("date {index}"), json!(null)),
                1 => attachment(id, "instructions", &format!("rules {index}"), json!(null)),
                2 => attachment(
                    id,
                    queued_command::SOURCE,
                    &format!("steer {index}"),
                    json!({queued_command::PROMPT_KEY: "steer", queued_command::MODE_KEY: queued_command::PROMPT_MODE}),
                ),
                3 => attachment(
                    id,
                    queued_command::SOURCE,
                    &format!("done {index}"),
                    json!({queued_command::PROMPT_KEY: "done", queued_command::MODE_KEY: queued_command::TASK_NOTIFICATION}),
                ),
                4 => attachment(
                    id,
                    "file",
                    &format!("image {index}"),
                    json!({rust_cli_kernel_api::message::ATTACHMENT_BLOCKS: [{"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBO"}}]}),
                ),
                5 => attachment(id, "max_turns_reached", "", json!({"maxTurns": 1})),
                _ => attachment(id, "todo_reminder", &format!("todo {index}"), json!(null)),
            }
        } else {
            Message::System(SystemMessage::Interrupted {
                id: MessageId::new(id),
            })
        };
        out.push(message);
    }
    out
}

fn options() -> Vec<NormalizeOptions> {
    let mut out = Vec::new();
    for system_turns in [true, false] {
        for turn_effort in [None, Some("high".to_owned())] {
            out.push(NormalizeOptions {
                system_turns,
                turn_effort,
                skip_cache_write: false,
            });
        }
    }
    out
}

/// 每一步：上一步定下的前缀 + 从上一步的切点接着规范化 = 整段规范化，前缀之后不再变（§4）。
#[test]
fn the_settled_prefix_and_the_tail_equal_the_whole_at_every_step() {
    let (mut checked, mut resumed) = (0, 0);
    for seed in 1..=240 {
        let messages = history(seed, 48);
        for options in options() {
            let mut settled: Vec<ApiMessage> = Vec::new();
            let mut cut: Option<(usize, Resume)> = None;
            for length in 1..=messages.len() {
                let all: Vec<&Message> = messages[..length].iter().collect();
                let whole = normalize(all.iter().copied(), &options);
                let (start, resume) = cut.take().unwrap_or_default();
                resumed += usize::from(start > 0);
                let (tail, next) = normalize_from(&all[start..], &options, resume, true);
                let joined: Vec<ApiMessage> = settled.iter().cloned().chain(tail.clone()).collect();
                assert_eq!(
                    joined, whole,
                    "seed {seed}, length {length}, options {options:?}"
                );
                // 从切点接着规范化时总有切点（那条 assistant 本身就是一个）；从头时没有 assistant 就没有。
                match next {
                    Some(next) => {
                        settled.extend(tail[..next.settled].iter().cloned());
                        cut = Some((start + next.source, next.resume));
                    }
                    None => assert_eq!(start, 0, "seed {seed}, length {length}"),
                }
                checked += 1;
            }
        }
    }
    assert!(checked > 40_000);
    // 大多数步是接着切点规范化的（不是每一步都从头）。
    assert!(resumed * 10 > checked * 8, "{resumed} / {checked}");
}

/// 切点之前的输出没有缓存断点：断点总在尾部（§4 第 7 条）。
#[test]
fn the_cache_breakpoint_is_never_in_the_settled_prefix() {
    for seed in 1..=60 {
        let messages = history(seed, 40);
        let all: Vec<&Message> = messages.iter().collect();
        let (out, cut) = normalize_from(&all, &options()[0], Resume::default(), true);
        if let Some(cut) = cut {
            assert!(
                out[..cut.settled]
                    .iter()
                    .flat_map(|message| &message.content)
                    .all(|content| content.cache_control.is_none()),
                "seed {seed}"
            );
        }
    }
}

const OPTIONS: NormalizeOptions = NormalizeOptions {
    system_turns: true,
    turn_effort: None,
    skip_cache_write: false,
};

/// 模型端口的替身：前缀的「编码」是每条消息的 JSON，按 `,` 接起来。
fn encode(messages: &[ApiMessage]) -> String {
    messages
        .iter()
        .map(|message| serde_json::to_string(message).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(",")
}

/// 一步：组装、按计划拼（替身的编码）、与整段比、交回新前缀（`keep` 为假时模拟没发出）。返回这一步有没有用上前缀。
async fn step(
    table: &Arc<RequestPrefixes>,
    session: &SessionId,
    history: &Arc<Vec<Message>>,
    tool_names: &[String],
    keep: bool,
) -> bool {
    let artifacts: Arc<dyn ArtifactStore> = Arc::new(MemoryArtifacts::default());
    let active = rust_cli_kernel_api::history::active(history);
    let sources = sources(&active, ToolSearchMode::Off, tool_names);
    let sources: Vec<&Message> = sources.iter().map(AsRef::as_ref).collect();
    let input = StepInput {
        session,
        table,
        history,
        tool_search: ToolSearchMode::Off,
        tool_names,
        options: &OPTIONS,
        artifacts: &artifacts,
    };
    let (messages, plan) = messages(input, &sources).await;
    let plan = plan.0;
    let whole = normalize(sources.iter().copied(), &OPTIONS);
    assert_eq!(plan.full().await, whole);
    let wire = plan.wire().cloned().unwrap_or_default();
    let spliced: Vec<String> = wire
        .chunks
        .iter()
        .map(ToString::to_string)
        .chain([encode(&messages)])
        .filter(|piece| !piece.is_empty())
        .collect();
    assert_eq!(spliced.join(","), encode(&whole));
    if keep {
        let settle = plan.settle();
        let mut next = WirePrefix {
            identity: 1,
            ..wire
        };
        if settle > 0 {
            next.chunks.push(Arc::from(encode(&messages[..settle])));
            next.messages += settle;
        }
        plan.keep(next);
    }
    plan.wire().is_some()
}

fn grown(base: &[Message], seed: u64, length: usize) -> Arc<Vec<Message>> {
    let mut out = base.to_vec();
    out.extend(history(seed, length).into_iter().map(|message| {
        let renamed = |id: &MessageId| MessageId::new(format!("{seed}-{}", id.as_str()));
        match message {
            Message::User(mut user) => {
                user.id = renamed(&user.id);
                Message::User(user)
            }
            Message::Assistant(mut assistant) => {
                assistant.id = renamed(&assistant.id);
                Message::Assistant(assistant)
            }
            Message::Attachment(mut attachment) => {
                attachment.id = renamed(&attachment.id);
                Message::Attachment(attachment)
            }
            Message::System(SystemMessage::Interrupted { id }) => {
                Message::System(SystemMessage::Interrupted { id: renamed(&id) })
            }
            other => other,
        }
    }));
    Arc::new(out)
}

/// 失效条件（§5）：只追加时接着用；回退、压缩、换了可用工具、上一步没交回时整段重建，之后又接着用。
#[tokio::test]
async fn the_table_rebuilds_on_rewind_compaction_key_change_and_lost_steps() {
    let table = Arc::new(RequestPrefixes::default());
    let session = SessionId::new("sess_prefix");
    let names = vec!["Read".to_owned()];
    let mut messages = history(7, 30);
    // 第一步整段，之后只追加：接着用。
    let mut used = Vec::new();
    for length in [10, 14, 20, 30] {
        let current = Arc::new(messages[..length].to_vec());
        used.push(step(&table, &session, &current, &names, true).await);
    }
    assert_eq!(used, [false, true, true, true]);
    // 回退：截到第 8 条再接上别的消息。
    messages.truncate(8);
    let rewound = grown(&messages, 11, 12);
    assert!(!step(&table, &session, &rewound, &names, true).await);
    assert!(step(&table, &session, &grown(&rewound, 12, 6), &names, true).await);
    // 换了可用工具（MCP 连上）。
    let more = vec!["Read".to_owned(), "mcp__x__y".to_owned()];
    let current = grown(&rewound, 13, 4);
    assert!(!step(&table, &session, &current, &more, true).await);
    // 压缩：活动历史从新的边界开始。
    let mut compacted = current.to_vec();
    compacted.push(Message::System(SystemMessage::CompactBoundary(
        CompactBoundary {
            id: MessageId::new("boundary"),
            trigger: CompactTrigger::Auto,
            pre_tokens: 1,
            summary: MessageId::new("summary"),
            preserved: None,
            discovered_tools: Vec::new(),
        },
    )));
    compacted.push(user(
        "summary".to_owned(),
        vec![text("summary")],
        UserOrigin::CompactSummary,
    ));
    let compacted = grown(&compacted, 14, 10);
    assert!(!step(&table, &session, &compacted, &more, true).await);
    let longer = grown(&compacted, 15, 6);
    assert!(step(&table, &session, &longer, &more, false).await);
    // 上一步没交回（没发出）：整段重建。
    assert!(!step(&table, &session, &grown(&longer, 16, 4), &more, true).await);
    // 会话关闭：条目释放。
    table.forget(&session);
    assert!(!step(&table, &session, &grown(&longer, 17, 4), &more, true).await);
}

/// 物化：前缀之后的消息与整段一样读 Artifact（§4 的表）。
#[tokio::test]
async fn artifact_images_in_the_tail_are_materialized_like_the_whole() {
    let artifacts = Arc::new(MemoryArtifacts::default());
    let session = SessionId::new("sess_images");
    let stored = artifacts
        .put(
            &session,
            rust_cli_kernel_api::store::ArtifactPurpose::Media,
            "text/plain".to_owned(),
            b"data:image/png;base64,iVBORw0KGgo=".to_vec(),
        )
        .await
        .expect("artifact");
    let image = ContentBlock::Image {
        source: ImageSource::Artifact {
            media_type: "image/png".to_owned(),
            artifact: stored,
        },
        origin: None,
    };
    let mut messages = history(3, 12);
    messages.push(user(
        "img".to_owned(),
        vec![text("see"), image],
        UserOrigin::Human,
    ));
    messages.extend(history(4, 8).into_iter().map(|message| match message {
        Message::Assistant(mut assistant) => {
            assistant.id = MessageId::new(format!("x{}", assistant.id.as_str()));
            Message::Assistant(assistant)
        }
        other => other,
    }));
    let table = Arc::new(RequestPrefixes::default());
    let store: Arc<dyn ArtifactStore> = artifacts;
    for length in [12, 15, messages.len()] {
        let history = Arc::new(messages[..length].to_vec());
        let active = rust_cli_kernel_api::history::active(&history);
        let sources = sources(&active, ToolSearchMode::Off, &[]);
        let sources: Vec<&Message> = sources.iter().map(AsRef::as_ref).collect();
        let input = StepInput {
            session: &session,
            table: &table,
            history: &history,
            tool_search: ToolSearchMode::Off,
            tool_names: &[],
            options: &OPTIONS,
            artifacts: &store,
        };
        let (tail, plan) = super::messages(input, &sources).await;
        let mut whole = normalize(sources.iter().copied(), &OPTIONS);
        crate::materialize::materialize(&mut whole, store.as_ref()).await;
        let prefix = plan.0.wire().map_or(0, |wire| wire.messages);
        assert_eq!(tail, whole[prefix..], "length {length}");
        assert_eq!(plan.0.full().await, whole);
        let settle = plan.0.settle();
        let mut next = plan.0.wire().cloned().unwrap_or_default();
        next.messages += settle;
        plan.0.keep(next);
    }
}
