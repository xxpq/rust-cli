//! 会话当前状态的读取（编排在 node-records `state`）；这里保留本机 SQLite 的当前轮查询。
#[cfg(test)]
use rust_cli_node_records::state::current_user;

// 性能修复（L21，docs/specs/rust-cli/performance.md §6）：原来是一条 SQL，按
// `sequence is null desc, sequence desc, …` 排序。这个顺序用不上索引，SQLite 先把会话的全部消息
// 逐条解析 JSON 过滤，再整体排序取第一条；每一步落库要调用 2–3 次，耗时随整个历史线性增长。
// 拆成两条：先看没有 sequence 的旧行（原顺序里它们排最前），再沿 `message_session_sequence_idx`
// 倒序扫，遇到第一条真实用户输入就停，结果与原来相同。实测 24 个提示 × 20 次 Read（2.7 MB）的会话，
// 最后一个提示的每步客户端开销中位数从 11.8 ms 降到 9.0 ms。
const CURRENT_USER_UNSEQUENCED: &str = "select id, data from message
          where session_id = ?1 and sequence is null
          and (case when json_valid(data) then json_extract(data, '$.role') end) = 'user'
          and (case when json_valid(data) then json_extract(data, '$.semantics.origin') end) = 'real_user'
          order by time_created desc, rowid desc limit 1";
const CURRENT_USER_SEQUENCED: &str = "select id, data from message
          where session_id = ?1 and sequence is not null
          and (case when json_valid(data) then json_extract(data, '$.role') end) = 'user'
          and (case when json_valid(data) then json_extract(data, '$.semantics.origin') end) = 'real_user'
          order by sequence desc, time_created desc, rowid desc limit 1";

/// `current_user` 依次执行的查询：前一条有结果就不再执行后面的（测试检查它们的查询计划）。
pub(crate) const CURRENT_USER_QUERIES: [&str; 2] =
    [CURRENT_USER_UNSEQUENCED, CURRENT_USER_SEQUENCED];

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
