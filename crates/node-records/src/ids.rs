//! Node 会话库的 id 与 slug 格式（Node `contracts/src/interfaces/shared.ts`、`core/src/runtime/helpers/project.ts`）。
//! 从 node-store `ids.rs` 移来（pg-store.md §12）；生成随机 uuid 的 `NodeIds` 仍在 node-store。
use rust_cli_base::js::base36;

/// 记录 id 里随机的部分（[`message_id`]、[`part_id`]、[`turn_id`] 的 `uuid`）从哪里来。本机与云端的会话库用同一个（node-store 的
/// `NodeIds`，全工程唯一生成随机 uuid 的地方）：同一个来源写出的记录逐字节相同（pg-store.md §12.3 的对拍）。
pub trait UuidSource: Send + Sync {
    fn uuid(&self) -> String;
}

/// Node `createMessageId`：`msg_<毫秒的 36 进制>_<uuid>`。
pub fn message_id(now_ms: u64, uuid: &str) -> String {
    format!("msg_{}_{uuid}", base36(now_ms))
}

/// Node `createPartId`：`part_<毫秒的 36 进制>_<uuid>`。
pub fn part_id(now_ms: u64, uuid: &str) -> String {
    format!("part_{}_{uuid}", base36(now_ms))
}

/// Node `createTurnId`。
pub fn turn_id(uuid: &str) -> String {
    format!("turn_{uuid}")
}

/// `msg_<毫秒的 36 进制>_<uuid>`（Node `createMessageId`）里的毫秒；格式不符时为 `None`。解析只定义在 kernel-api。
pub fn message_time(id: &str) -> Option<u64> {
    rust_cli_kernel_api::ids::message_created_ms(id)
}

/// 消息的创建时间：内核的消息 id 是 `msg_<创建毫秒>_<uuid>`（与 Node 相同），取其中的毫秒；
/// 格式不符或晚于提交时间（时钟回拨）时取提交时间。
pub fn created_at(id: &str, now: u64) -> u64 {
    message_time(id).filter(|at| *at <= now).unwrap_or(now)
}

/// `[^a-z0-9._-]+` 换成 `-`，再去掉首尾的 `-`。
fn dashed(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut dash = false;
    for c in value.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').to_owned()
}

/// Node core `slugify`：结果为空时是 `session`。
pub fn slugify(value: &str) -> String {
    let slug = dashed(value);
    if slug.is_empty() {
        "session".into()
    } else {
        slug
    }
}

/// 项目 ID 里 slug 的最多字符数。
const MAX_PROJECT_SLUG_LEN: usize = 80;

/// Node core `projectIdFromDirectory`：`proj_` 加 slug 的前 80 个字符（slug 只有 ASCII，按字节截取
/// 等于 JS `slice`）。
pub fn project_id(directory: &str) -> String {
    let slug = slugify(directory);
    format!("proj_{}", &slug[..slug.len().min(MAX_PROJECT_SLUG_LEN)])
}
