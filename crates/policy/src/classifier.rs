//! 判决链的缺省分类器（端口在 kernel-api 的 `classifier`，docs/specs/rust-cli/auto-classifier.md §15.2）。
//!
//! 产品里 runtime 注入 classifier crate 的实现（§16.9）；没有注入时（测试、嵌入）用 [`NoClassifier`]，结论是“判不了”，判决链按
//! 解析不了拦下（`CLASSIFIER_UNAVAILABLE`）。

use async_trait::async_trait;
use rust_cli_kernel_api::classifier::{
    AutoClassifier, ClassifierVerdict, ClassifyRequest, NoVerdict,
};

/// 没有分类器。
pub struct NoClassifier;

#[async_trait]
impl AutoClassifier for NoClassifier {
    async fn classify(&self, _request: ClassifyRequest<'_>) -> ClassifierVerdict {
        ClassifierVerdict::NoVerdict(NoVerdict::Unparseable)
    }
}
