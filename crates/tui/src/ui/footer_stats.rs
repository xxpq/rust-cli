//! 请求统计（docs/specs/rust-cli/tui-display.md §2.6，修复线 F27）：状态栏的缓存命中率的来源。值只来自内核：用量表的查询结果，
//! 由 runner 按序号写入 [`FooterStats`]。以前 `/stats` 卡的速度一行也读这里（每份统计记进速度的历史）；卡 2026-11-12 删掉
//! （tui-panels.md §4），历史一起删掉。
use rust_cli_kernel_api::metering::RequestStats;

/// runner 写入的最后一份请求统计。回执带请求序号：切换会话与 `/clear` 时 [`Self::clear`] 让在途的回执作废，不用超时。
#[derive(Debug, Default)]
pub struct FooterStats {
    seq: u64,
    stats: RequestStats,
}

impl FooterStats {
    /// 发一次新查询：序号加一并返回。
    pub fn begin(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// 查询的回执：序号是最新的才收下（返回是否收下）；查询失败（存储不支持）时当作没有值。
    pub fn arrived(&mut self, seq: u64, stats: Option<RequestStats>) -> bool {
        if seq != self.seq {
            return false;
        }
        self.stats = stats.unwrap_or_default();
        true
    }

    /// 离开会话：清空，在途的回执作废。
    pub fn clear(&mut self) {
        self.seq += 1;
        self.stats = RequestStats::default();
    }

    pub fn stats(&self) -> &RequestStats {
        &self.stats
    }
}
