//! Node 记录 → 内核消息（docs/specs/rust-cli/node-store.md §5）。
//!
//! 规则取自 Node `hydrateMessageHistoryFromSession` 的文本子集（复制来源
//! `apps/zcode-cli-rust/crates/domain/src/node_history/hydrate.rs`）：
//! - 本工程写的合成消息（attachment、meta、中断）按 `metadata.rustCli` 还原（[`crate::notices`]）；
//! - user：未忽略的非合成 text part，`metadata.modelInput` 优先；压缩摘要（`summary`）连合成 part
//!   一起读；Node 自己的提醒（只有合成 part）、未接入的共享上下文跳过。
//! - assistant：reasoning part → thinking 块（`metadata.anthropic.signature` / `redactedData`），
//!   未忽略的非空 text part → text 块，tool part → tool_use 块；没有任何块时跳过。已结束的 tool part
//!   拆成工具结果消息（[`crate::tool_parts`]），排在位置键那一行之后，没有位置键时紧跟 assistant；Node 写的（没有本工程
//!   标记的）未结调用在内存里补 Node 的中断结果，按声明顺序夹在 Node 写的结果中间（工作线 F86、F88）。续写占位还原成内核
//!   的占位（`is_placeholder`）。
//! - 文件、agent、时间线等 part 不读。
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, NO_RESPONSE_REQUESTED, SYNTHETIC_MODEL, StopReason,
    ToolResultContent, Usage, UserMessage, UserOrigin,
};
use serde_json::Value;

use crate::branch::truthy;
use crate::row::WithParts;
use crate::tool_parts;

pub fn hydrate(active: &[Cow<'_, WithParts>]) -> Vec<Message> {
    assemble(
        active
            .iter()
            .map(|record| (record.id(), piece(record)))
            .collect(),
    )
}

/// 一条消息记录换出的内核消息（还没拼，node-store.md §5.4）：先是它自己的（压缩边界、user / assistant），再是它拆出来的结果与各自的位置键。
/// 只看这一条记录：冷读取逐条解码时换完就丢掉这一条的 JSON。
#[derive(Debug, Clone, Default)]
pub struct Piece {
    own: Vec<Message>,
    results: Vec<(Option<String>, Message)>,
}

impl Piece {
    /// 这条记录换出的消息里有没有开回合的用户输入（没有回合记录的历史按它切回合，node-store.md §5.6）。
    pub fn opens_turn(&self) -> bool {
        self.own
            .iter()
            .any(rust_cli_kernel_api::turn_walk::opens_turn)
    }
}

pub fn piece(record: &WithParts) -> Piece {
    let mut piece = Piece::default();
    match record.info["role"].as_str() {
        Some("user") => {
            // 压缩摘要之前还原边界（docs/specs/rust-cli/compact.md §6）。
            if truthy(record.info.get("summary")) {
                piece.own.extend(crate::compaction::hydrate_boundary(
                    &record.parts,
                    record.id(),
                ));
            }
            piece
                .own
                .extend(crate::notices::hydrate(record).or_else(|| user(record)));
        }
        Some("assistant") => {
            let tools = tool_parts::selected(&record.parts);
            piece.own.extend(assistant(record, &tools));
            piece.results = tool_parts::results(&MessageId::new(record.id()), &tools);
        }
        _ => {}
    }
    piece
}

/// 按活动历史的顺序把片段接起来：结果按位置键排在它指向的那一行之后（那一行还没走到时才等，node-store.md §4.6），没有位置键的紧跟它的 assistant。
pub fn assemble(order: Vec<(&str, Piece)>) -> Vec<Message> {
    let mut out = Vec::with_capacity(order.len());
    // 等着排在某一行之后的结果；还没走到的行才能等。
    let ahead: HashSet<&str> = order.iter().map(|(id, _)| *id).collect();
    let mut waiting: HashMap<String, Vec<Message>> = HashMap::new();
    let mut passed: HashSet<&str> = HashSet::new();
    for (id, piece) in order {
        passed.insert(id);
        out.extend(piece.own);
        for (after, result) in piece.results {
            match after {
                Some(row) if ahead.contains(row.as_str()) && !passed.contains(row.as_str()) => {
                    waiting.entry(row).or_default().push(result);
                }
                _ => out.push(result),
            }
        }
        out.extend(waiting.remove(id).unwrap_or_default());
    }
    out
}

fn visible_text(part: &Value) -> bool {
    part["type"] == "text" && part["ignored"] != true
}

/// Node `textPartToProviderText` 的非合成分支：`/init` 与自定义命令的模型输入在 `metadata.modelInput`。
fn provider_text(part: &Value) -> &str {
    if part["synthetic"] != true
        && let Some(input) = part["metadata"]["modelInput"].as_str()
    {
        return input;
    }
    part["text"].as_str().unwrap_or("")
}

fn user(record: &WithParts) -> Option<Message> {
    let info = &record.info;
    let shared = &info["metadata"]["sharedContextStatus"];
    if info["source"] == "shared_context" && !shared.is_null() && *shared != "attached" {
        // 共享上下文在首次发送前只是候选，不能提前进入模型上下文。
        return None;
    }
    let summary = truthy(info.get("summary"));
    let content: Vec<ContentBlock> = record
        .parts
        .iter()
        .filter_map(|part| {
            // 人类输入的图片（Node `file` part，tui-input.md §3.2）与 PDF（node-store.md §4.4.2）按 part 的顺序读回；其他附件与
            // 读不出的媒体仍然不读（node-store.md §5）。
            if part["type"] == "file" {
                // 用户附上的是哪个文件（node-store.md §4.4.2）：本工程与 Node 写的同一个键
                let origin = crate::tool_content::origin_of(part);
                return match crate::tool_content::from_file_part(part) {
                    ToolResultContent::Image { source } if !summary => {
                        Some(ContentBlock::Image { source, origin })
                    }
                    ToolResultContent::Document { source } if !summary => {
                        Some(ContentBlock::Document { source, origin })
                    }
                    _ => None,
                };
            }
            if !(visible_text(part) && (summary || part["synthetic"] != true)) {
                return None;
            }
            let text = provider_text(part);
            (!text.is_empty()).then(|| ContentBlock::Text {
                text: text.to_owned(),
            })
        })
        .collect();
    let has_content = content.iter().any(|block| match block {
        ContentBlock::Text { text } => !rust_cli_base::js::string::trim(text).is_empty(),
        _ => true,
    });
    if !has_content {
        return None;
    }
    Some(Message::User(UserMessage {
        id: MessageId::new(record.id()),
        content,
        origin: if summary {
            UserOrigin::CompactSummary
        } else {
            UserOrigin::Human
        },
    }))
}

fn thinking(part: &Value) -> ContentBlock {
    let anthropic = &part["metadata"]["anthropic"];
    if let Some(data) = anthropic["redactedData"].as_str() {
        return ContentBlock::RedactedThinking {
            data: data.to_owned(),
        };
    }
    ContentBlock::Thinking {
        thinking: part["text"].as_str().unwrap_or("").to_owned(),
        signature: anthropic["signature"].as_str().map(str::to_owned),
    }
}

/// assistant：推理与文本块按 part 顺序，tool_use 块排在后面（Node `addAssistant` 把工具调用单独放在
/// 文本之后）。
fn assistant(record: &WithParts, tools: &[&Value]) -> Option<Message> {
    let mut content: Vec<ContentBlock> = record
        .parts
        .iter()
        .filter_map(|part| match part["type"].as_str() {
            Some("reasoning") => Some(thinking(part)),
            Some("text") if visible_text(part) => part["text"]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(|text| ContentBlock::Text {
                    text: text.to_owned(),
                }),
            _ => None,
        })
        .collect();
    content.extend(tools.iter().map(|part| tool_parts::tool_use(part)));
    if content.is_empty() {
        return None;
    }
    let info = &record.info;
    if is_placeholder(info, &content) {
        return Some(Message::Assistant(AssistantMessage {
            id: MessageId::new(record.id()),
            content,
            model: SYNTHETIC_MODEL.to_owned(),
            stop_reason: Some(StopReason::StopSequence),
            usage: None,
        }));
    }
    Some(Message::Assistant(AssistantMessage {
        id: MessageId::new(record.id()),
        content,
        model: info["modelId"].as_str().unwrap_or("").to_owned(),
        stop_reason: info["finish"].as_str().map(stop_reason),
        usage: usage(&info["tokens"]),
    }))
}

/// 续写占位（node-store.md §4.2.1，工作线 F77）：只给模型看的 assistant，唯一的块是 `No response requested.`。
/// 别的只给模型看的 assistant（Node 副屏继承的历史）照常是回复。
fn is_placeholder(info: &Value, content: &[ContentBlock]) -> bool {
    let semantics = &info["semantics"];
    semantics["uiVisibility"] == "hidden"
        && semantics["providerVisibility"] == "visible"
        && matches!(content, [ContentBlock::Text { text }] if text == NO_RESPONSE_REQUESTED)
}

/// AI SDK `finishReason` → Anthropic `stop_reason`（`stop` 读回 end_turn，见 spec 的已知差异）。
fn stop_reason(finish: &str) -> StopReason {
    match finish {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool-calls" => StopReason::ToolUse,
        "content-filter" => StopReason::Refusal,
        _ => StopReason::Unknown,
    }
}

/// Node `tokens` → Anthropic `usage`：`input` 含缓存读写，拆回不含缓存的 `input_tokens`。全为 0
/// （Node `emptyTokenUsageInfo`）时没有用量。
fn usage(tokens: &Value) -> Option<Usage> {
    let count = |value: &Value| value.as_u64().unwrap_or(0);
    let input = count(&tokens["input"]);
    let read = count(&tokens["cache"]["read"]);
    let write = count(&tokens["cache"]["write"]);
    let output = count(&tokens["output"]);
    if input == 0 && read == 0 && write == 0 && output == 0 {
        return None;
    }
    Some(Usage {
        input_tokens: input.saturating_sub(read.saturating_add(write)),
        output_tokens: output,
        cache_creation_input_tokens: write,
        cache_read_input_tokens: read,
    })
}
