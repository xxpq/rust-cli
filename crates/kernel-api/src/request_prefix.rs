//! 主线程请求的已定前缀（docs/specs/rust-cli/request-prefix.md）：主循环与模型端口之间的契约。
//!
//! 主循环（query）拥有前缀的事实：源前缀、规范化的接续状态与按会话的表；模型端口（model）拥有线编码。一次请求里主循环给出
//! [`PrefixPlan`]（放在 [`crate::model::RequestOptions::prefix`]）：这次请求的 `messages` 之前已经编码好的前缀、这次的
//! `messages` 有几条并入下一步的前缀、整段重建的入口与交回新前缀的入口。模型端口按这次端点的编码身份决定拼前缀还是整段编码，
//! 编码之后把新前缀交回（[`PrefixPlan::keep`]）。

use std::fmt;
use std::sync::Arc;

use futures_util::future::BoxFuture;

use crate::model::ApiMessage;

/// 模型端口写好的消息前缀：请求消息的前 [`Self::messages`] 条按线协议写成的文本（条与条之间的 `,` 在文本里，不含外层的
/// `[`、`]`）。文本按段存：每一步追加一段，不复制旧段（request-prefix.md §6）。
///
/// `identity` 与 `flags` 的含义由模型端口定义（编码身份：协议与改变消息字节的端点事实；特性位：beta 的判定要看的消息特性），
/// 这里只保存、原样交回。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WirePrefix {
    pub identity: u32,
    pub chunks: Vec<Arc<str>>,
    pub messages: usize,
    pub flags: u32,
}

impl WirePrefix {
    /// 文本的总字节数（不含段之间的分隔：段与段之间由写出方补 `,`）。
    pub fn text_len(&self) -> usize {
        self.chunks.iter().map(|chunk| chunk.len()).sum()
    }
}

/// 一次请求的前缀计划（request-prefix.md §3）。主循环实现；模型端口在取端点之后用它，最多调一次 [`Self::keep`]。
pub trait PrefixPlan: Send + Sync {
    /// 这次请求的 `messages` 之前已经编码好的前缀；没有时 `messages` 是整段。
    fn wire(&self) -> Option<&WirePrefix>;

    /// 这次请求的 `messages` 的前几条并入下一步的前缀。整段重建时（[`Self::full`]）是前缀的条数加上它：整段的消息就是
    /// 前缀的那几条接着这次的 `messages`（request-prefix.md §4）。
    fn settle(&self) -> usize;

    /// 整段的消息（与没有前缀时主循环发出的相同）：编码身份与 [`Self::wire`] 不同、或这次不能拼前缀时用它。
    fn full(&self) -> BoxFuture<'static, Vec<ApiMessage>>;

    /// 交回编码之后的新前缀（下一步用）。不调用时下一步整段重建。
    fn keep(&self, prefix: WirePrefix);
}

/// [`crate::model::RequestOptions::prefix`] 的值：同一个计划的共享引用。只在内存，不序列化；相等按引用。
#[derive(Clone)]
pub struct PlanRef(pub Arc<dyn PrefixPlan>);

impl fmt::Debug for PlanRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let wire = self.0.wire();
        formatter
            .debug_struct("PlanRef")
            .field("wire_messages", &wire.map(|wire| wire.messages))
            .field("settle", &self.0.settle())
            .finish()
    }
}

impl PartialEq for PlanRef {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
