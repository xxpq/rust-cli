//! 本 crate 的命名常量（有含义的值集中在这里，docs/specs/rust-cli/constants.md）。

use rust_cli_base::units;

/// 本端提供的能力（protocol-v5.md §5、§15.7、§16、§36、§38、§39、§53、§55、§59、§60、§65）：握手外壳、agent、会话核心与队列编辑、工作区 MCP 的信任、外部账号层、这一轮的执行覆盖、附件的上传、改写历史、fork、
/// 插件引用目录与 hooks 预信任、`/` 面板的命令目录、归档会话的永久删除。只声明做完了的
/// （03 §1.1）；同一能力里还没做的命令回 `precondition.capabilityUnsupported`。修订取生成的能力表。
pub const SERVED_CAPABILITIES: &[&str] = &[
    CORE_CAPABILITY,
    CORE_PATCH_CAPABILITY,
    AGENT_CAPABILITY,
    SESSION_CAPABILITY,
    SESSION_QUEUE_CAPABILITY,
    MCP_TRUST_CAPABILITY,
    ACCOUNT_OVERLAY_CAPABILITY,
    SESSION_EXECUTION_CAPABILITY,
    ATTACHMENTS_CAPABILITY,
    SESSION_REWIND_CAPABILITY,
    SESSION_FORK_CAPABILITY,
    CATALOG_PLUGINS_REFERENCE_CAPABILITY,
    HOOKS_PRETRUST_CAPABILITY,
    CATALOG_SLASH_CAPABILITY,
    SESSION_DELETION_CAPABILITY,
];
pub const CORE_CAPABILITY: &str = "core";
/// 增量 op `entity.patch`（01 §19.4，protocol-v5.md §68）：协商出它的连接收到 patch，别的连接收到同一项当前整行的 put。
pub const CORE_PATCH_CAPABILITY: &str = "core.patch";
pub const AGENT_CAPABILITY: &str = "agent";
pub const SESSION_CAPABILITY: &str = "session";
pub const SESSION_QUEUE_CAPABILITY: &str = "session.queue";
/// 改写历史：编辑重发、重试、回退文件与预览（protocol-v5.md §53）。
pub const SESSION_REWIND_CAPABILITY: &str = "session.rewind";
/// fork：`turn.fork`（protocol-v5.md §55）。
pub const SESSION_FORK_CAPABILITY: &str = "session.fork";
/// `/` 面板里内核的命令：`slash.commands`（protocol-v5.md §60）。
pub const CATALOG_SLASH_CAPABILITY: &str = "catalog.slash";
pub const MCP_TRUST_CAPABILITY: &str = "mcp.trust";
/// 归档会话的永久删除：Host 专用的 `sessionData.deletionPreview` / `deletionExecute`（§65）。`session.purge`（项目数据的测量与清除）不声明。
pub const SESSION_DELETION_CAPABILITY: &str = "session.deletion";
/// 对话引用的插件目录：`plugins.referenceCatalog*`（§59.1）。界面据它给 @ 插件；插件管理（`catalog.plugins`）不声明。
pub const CATALOG_PLUGINS_REFERENCE_CAPABILITY: &str = "catalog.plugins.reference";
/// 远程 MCP 服务器的授权（§74）：不在 [`SERVED_CAPABILITIES`] 里，入口装了授权（`Settings.mcp_oauth`）才在握手时声明。
pub const MCP_OAUTH_CAPABILITY: &str = "mcp.oauth";
/// 插件管理（§72）：不在 [`SERVED_CAPABILITIES`] 里，入口交了端口 `PluginAdmin` 才在握手时声明。
pub const CATALOG_PLUGINS_CAPABILITY: &str = "catalog.plugins";
/// `operations` 里终态的项最多留这么多条（同 `@zcode/protocol` 的 `PLUGIN_OPERATIONS_RETAINED`，24 §6）。大小预算，不是计时器。
pub const PLUGIN_OPERATIONS_RETAINED: usize = 32;
/// 工作区级的 hooks 预信任：`hooks.trustGrant`（§59.2）。会话里的审查 flow（`workspace.config`）不声明。
pub const HOOKS_PRETRUST_CAPABILITY: &str = "hooks.pretrust";
pub const ACCOUNT_OVERLAY_CAPABILITY: &str = "account.overlay";
/// 这一轮的执行覆盖（`input.send.modelExecution`，§38）：界面据它决定抽不抽卡（23 §8 第 1 项）。
pub const SESSION_EXECUTION_CAPABILITY: &str = "session.execution";
/// 附件的上传与读面（§39）：界面据它决定粘贴的图片能不能上传（15 §13.1）。
pub const ATTACHMENTS_CAPABILITY: &str = "attachments";
/// 同时进行中的上传（同 `@zcode/protocol` 的 `ATTACHMENT_LIMITS.uploadMaxConcurrent`，§39.3）。大小预算，不是计时器。
pub const UPLOAD_MAX_CONCURRENT: usize = 16;
/// 一块 chunk 解码之后的字节上限（同 `ATTACHMENT_LIMITS.chunkMaxBytes`）。
pub const CHUNK_MAX_BYTES: usize = 512 * units::KIB;
/// 全部上传已收的字节的总预算（同 `ATTACHMENT_LIMITS.uploadMaxStagedBytes`，§41.5）：在 chunk 时守。
pub const UPLOAD_MAX_STAGED_BYTES: usize = 64 * units::MIB;
/// 读面读一个本机附件的上限（同 `ATTACHMENT_LIMITS.maxBytes`；视频是 `previewMaxBytes`，§41.4）。
pub const ATTACHMENT_READ_MAX_BYTES: u64 = 20 * units::MIB as u64;
pub const ATTACHMENT_PREVIEW_MAX_BYTES: u64 = 30 * units::MIB as u64;
/// 每份会话快照带的尾部行数（02 §2.3；同 `@zcode/protocol` 的 `SESSION_SNAPSHOT_ROWS`）。大小预算，不是计时器。
pub const SESSION_SNAPSHOT_ROWS: usize = 60;
/// 会话快照的尾部窗口与 `rows.range` 一页的字节预算（01 §19.5，用户定 R4；同 `@zcode/protocol` 的 `SESSION_SNAPSHOT_BYTES`）：累计的行字节
/// 不超过它，至少一行。大小预算，不是计时器。
pub const SESSION_SNAPSHOT_BYTES: usize = 512 * units::KIB;
/// 冷读只读最后这么多条存储消息（再往前补到回合开头，protocol-v5.md §73.2）：一条消息至少一行的情形下够快照的尾窗。
pub const TAIL_RECORDS: usize = SESSION_SNAPSHOT_ROWS;
/// 只建了后缀的投影的第一行的号（§73.1）：往前补的段往前编，下沿之前的行再多也不会把号挤到 0 以下；加上活会话的新行也远在 JS 的安全整数之内。
pub const ROW_NUMBER_ANCHOR: u64 = 1 << 32;
/// 问题导航的目录一页读这么多条存储消息（§73.5）。
pub const PROMPTS_PAGE_RECORDS: usize = 400;
/// 目录里提问文字与回答文字的开头，至多这么多个字符（契约 `SESSION_PROMPTS_LIMITS`，§73.5）。
pub const PROMPT_TEXT_CHARS: usize = 400;
pub const PROMPT_ANSWER_CHARS: usize = 400;
/// 工具输出在行上的预览（01 §19.5，用户定 R4）：全文的 UTF-8 字节数超过它时 `output.text` 只带开头、`output.truncated.tail` 带结尾，
/// 各一半。完整文字走 `tool.output`。
pub const TOOL_OUTPUT_PREVIEW_BYTES: usize = 16 * units::KIB;
/// 一条连接上同时开着运行时的会话数的上限（用户定 2026-11-11，protocol-v5.md §69）：超过时关最久没用且空闲的。大小预算，不是计时器。
pub const HOT_SESSIONS_MAX: usize = 8;
/// 一条连接上保留的冷会话投影的上限（用户定 2026-11-12，protocol-v5.md §70）：最后一个看它的人走了之后不立即释放，超过时释放最久没用的。
/// 与 [`HOT_SESSIONS_MAX`] 分开：那个限开着的运行时，这个限没人看的冷投影。大小预算，不是计时器。
pub const RETAINED_SESSIONS_MAX: usize = 8;
/// `inputs` 集合保留的终态项数（02 §1.5；同 `@zcode/protocol` 的 `INPUT_FATE_RETAIN`）。
pub const INPUT_FATE_RETAIN: usize = 100;
/// `work.bashOutput` 给的输出末尾的字节数（同 `@zcode/protocol` 的 `SESSION_QUERY_LIMITS.backgroundBashOutputMaxBytes`，§43.2）。
pub const BASH_OUTPUT_TAIL_BYTES: usize = 8 * units::KIB;
/// `session.debug` 保留的主回合轮数与网络条目数（同 `SESSION_QUERY_LIMITS.debugRounds` / `debugNetworkEntries`，§43.2）。
pub const DEBUG_ROUNDS: usize = 200;
pub const DEBUG_NETWORK_ENTRIES: usize = 100;

/// 本机链路（Host↔CLI，stdio）单条消息的上限（03 §3.2 的现值）：welcome 的 `limits.maxMessageBytes`，也是读帧的上限。
pub const MAX_MESSAGE_BYTES: usize = 16 * units::MIB;
/// 一次 `session.fileChanges` 的回复里全部 `patches` 的 JSON 字节合计的上限（protocol-v5.md §44.2）：回复是一条链路消息，不能超过
/// [`MAX_MESSAGE_BYTES`]，另一半留给明细的其余字段与信封。大小预算，不是计时器。
pub const FILE_PATCHES_MAX_BYTES: usize = MAX_MESSAGE_BYTES / 2;
/// 端到端信用的初值（帧数，03 §3.2 的现值）。
pub const INITIAL_CREDIT: u64 = 4;
/// 每个 topic 保留日志的字节预算（01 §19.2；同 protocol-runtime 的 `RETAINED_LOG_BYTES`）：日志只记谁变了与追加的块，超了丢最早的；
/// 只决定续传走日志路还是变更序号路，不决定对错。
pub const RETAINED_LOG_BYTES: usize = units::MIB;
/// 每个 topic 保留的墓碑（删掉的项）条数（同 protocol-runtime 的 `RETAINED_TOMBSTONES`）：丢掉的墓碑让更早的 cursor 拿快照。
pub const RETAINED_TOMBSTONES: usize = 1024;
/// 读循环交给 actor 的消息队列（条）：满了读循环等，不丢。
pub const INTAKE_QUEUE: usize = 256;
/// 一项 Gen UI 页面状态的字节上限（`state` 的 JSON 文字，02 §11.6、§12.8 K-P3；同 `@zcode/shared` 的 `GEN_UI_STATE_MAX_BYTES`）：
/// 执行方在命令入口守，超出 → `precondition.payloadTooLarge`。
pub const GEN_UI_STATE_MAX_BYTES: usize = 16 * units::KIB;
/// 一条输入全部页面状态的字节上限（`[{path, state}]` 的 JSON 文字；同 `GEN_UI_CONTEXT_MAX_BYTES`）。
pub const GEN_UI_CONTEXT_MAX_BYTES: usize = 64 * units::KIB;
/// 工具行运行中的输出预览（`outputPreview`，protocol-v5.md §19.4；v4 `executionOutputPreviewSchema` 的形状）：尾部缓冲的字节上限
/// （v4 的上限是 4096 个 UTF-16 单元，按字节截一定不超过它），`text` 与 `fullText` 的行数（照 v4 的 CLI）。
pub const OUTPUT_PREVIEW_MAX_BYTES: usize = 4 * units::KIB;
pub const OUTPUT_PREVIEW_SHORT_LINES: usize = 5;
pub const OUTPUT_PREVIEW_FULL_LINES: usize = 100;
/// 会话列表的种子条数（02 §11.2 C；同 `@zcode/protocol` 的 `SESSIONS_SNAPSHOT_FACTS`）：最近活动的这么多条列表会话（含软删除的），
/// 其余由 Host 的 task-facts 用 `sessions.facts` 按 ID 对账。大小预算，不是计时器。
pub const SESSIONS_SNAPSHOT_FACTS: usize = 200;

/// 取一次账号凭据的端口总时限（T2 `cli.service.requestAuthTimeout`，docs/zcode-protocol-v5/21-request-auth.md §3.3）：从发出
/// `host.providers.requestAuth` 起计，Host 的应答（与以后的用户完成校验）都在里面；照 Node 的端口时限。不是同步手段。
pub const REQUEST_AUTH_LIMIT: std::time::Duration = std::time::Duration::from_secs(180);

/// `agent` 的诊断字段 `implementation`（01 §17.4：只进日志与诊断）。
pub const IMPLEMENTATION: &str = "rust";
/// welcome 的 `server.kind`：Host↔CLI 链路的服务端是 CLI。
pub const SERVER_KIND: &str = "cli";

/// v5 的应用错误码：JSON-RPC 的 `error.data` 是 ProtocolError（protocol-runtime `link/core.ts` 的 PROTOCOL_ERROR_CODE）。
pub const PROTOCOL_ERROR_CODE: i64 = -31000;
