// 协议前端的解耦词表（README §3.1；从 policy.mjs 拆出）：每种协议的 crate、词汇、方法名，以及词汇的例外。
// 两张表一起变化（加一种协议就同时加它的词汇与例外），所以放在一起。

// ZCode App 协议的方法名表（PROTOCOLS.zcode，app-protocol.md §9.2）：由 scripts/rust-cli/app-protocol/inventory.mjs --write
// 从 App 的协议源码生成，--check 查漂移。
import ZCODE_METHODS from "./zcode-methods.json" with { type: "json" };

/// 每种协议：属于它的 crate、它的词汇（`re` 在源码文字上匹配）与方法名（字符串字面量与它完全相同才算）。
/// `comments`：注释里出现也算（ZCode 与 ACP 是；stream-json 是对齐对象自己的概念，内核注释里说明“-p / stream-json 输入
/// 时怎样”是照对齐对象，只查代码与字面量）。词汇只能出现在它自己的 crate 与入口（组装根解析子命令）里。
export const PROTOCOLS = {
  zcode: {
    crates: ["protocol-zcode"],
    comments: true,
    terms: [
      // sharedWith：两家 ZCode 协议共用的品牌词，也能出现在这些 crate（v5 也叫 ZCode Protocol，F141）。
      {
        id: "zcode-protocol",
        re: /zcode[_-]?protocol|ZCode ?Protocol/i,
        sharedWith: ["protocol-v5"],
      },
      { id: "app-server", re: /app[-_]server|AppServer|appServer/ },
      // 协议的名字 V4（大写、独立的词）；`Ipv4Addr`、`new_v4`、“uuid v4” 不算。
      { id: "v4-name", re: /(?<![A-Za-z0-9_])V4(?![A-Za-z0-9_])/ },
      // 方法名的前缀 `v4/`；URL 里的 `/v4/`（模型端点的路径）不算。
      { id: "v4-method", re: /(?<![A-Za-z0-9_/.:-])v4\/[A-Za-z]/ },
      { id: "log-epoch", re: /log[_-]?epoch/i },
      // 两种投递与它们的选择键。
      {
        id: "client-mode",
        re: /desktop[-_]continuous|web[-_]remote[-_]replayable|client[_-]?mode/i,
      },
      { id: "delivery-kind", re: /delivery[_-]?kind|wire[_-]?version/i },
      // 命令裁决的字段（CAS 与幂等表）：revision 由会话 actor 给出，裁决在前端（app-protocol.md §6）。
      {
        id: "command-cas",
        re: /revision[_-]?at[_-]?decision|base[_-]?revision|base[_-]?log[_-]?epoch|command[_-]?inbox/i,
      },
      { id: "topic", re: /sessions-index\/|workspace-config\// },
      {
        id: "snapshot-field",
        re: /pendingInteractions|inputRouting|pendingCommands|workspaceHookAdmission/,
      },
      // Host 的身份键（workspaceIdentity 是与 Node 共用的存储字段，不在这里）。v5 的命令目标与 topic 段也用它（01 §17.1）。
      { id: "workspace-key", re: /workspace[_-]?key/i, sharedWith: ["protocol-v5"] },
      // Host 给 agent 的环境变量与启动参数：由入口读，换成内核的通用参数。
      // （`ZCODE_AGENT_CA_CERT` 是代理与根证书的通用变量，TUI 也认，不在这里。）
      {
        id: "host-env",
        re: /ZCODE_WORKSPACE_IDENTITY|GEN_UI_OUTPUT_ROOT|ZCODE_AGENT_SERVER_[A-Z_]+|ZCODE_AGENT_(?:IMPL|RUST_BIN)\b/,
      },
      { id: "surface-flag", re: /--surface\b/ },
      { id: "electron", re: /ELECTRON_RUN_AS_NODE|utilityProcess|Electron/ },
    ],
    methods: ZCODE_METHODS.methods,
    // Host 与 agent 之间的方法（旧方法、V4、反向请求、通知）。
    methodId: "host-method",
  },
  // ZCode Protocol v5（docs/specs/rust-cli/protocol-v5.md §1，F141）：方法名（initialize、ping、subscribe……）是通用的词，不列。
  "zcode-v5": {
    crates: ["protocol-v5"],
    comments: true,
    terms: [
      { id: "v5-name", re: /protocol[-_ ]?v5|ZCode ?Protocol ?v5/i },
      // 增量 op（02 §2）的字符串（只认引号里的：`state.set(...)` 这类方法调用不算）
      { id: "v5-delta-op", re: /"(?:entity\.(?:put|append|remove)|rows\.truncate|state\.set)"/ },
      // Host↔CLI 的工作区级 topic（`ws/<key>/…`，01 §17.1）
      { id: "v5-workspace-topic", re: /(?<![A-Za-z0-9_/.:-])ws\/(?:<|%|\{)/ },
    ],
    methods: [],
  },
  acp: {
    crates: ["protocol-acp"],
    comments: true,
    terms: [
      {
        id: "acp-name",
        re: /(?<![A-Za-z0-9_])ACP(?![A-Za-z0-9_])|agent[-_ ]?client[-_ ]?protocol/i,
      },
      {
        id: "acp-field",
        re: /agentCapabilities|clientCapabilities|sessionUpdate|agent_message_chunk|agent_thought_chunk|tool_call_update|available_commands_update|current_mode_update/,
      },
    ],
    // `initialize`、`authenticate` 与 MCP 同名，不列。
    methods: [
      "session/new",
      "session/load",
      "session/prompt",
      "session/cancel",
      "session/update",
      "session/request_permission",
      "session/set_mode",
      "session/set_model",
      "fs/read_text_file",
      "fs/write_text_file",
      "terminal/create",
      "terminal/output",
      "terminal/release",
      "terminal/wait_for_exit",
      "terminal/kill",
    ],
  },
  // App 前端层的产品词（docs/specs/rust-cli/context.md §15.5，APP-CTX；app-protocol.md §9.2）：前端上下文段、Gen UI、visualize、评审卡、
  // 文件链接、`plugin://` 引用。内容只在入口 cli 的 App 前端层模块（`frontends/app/`，入口不查），内核经端口 `FrontendPrompt` 取；
  // 这一组没有自己的 crate，两个 App 协议 crate 的协议字段里有这些名字（`genUiContext` 一类），共用。只认这些写法：普通的英文
  // （genuine、code comments、the file links to）不算。
  "app-frontend": {
    crates: [],
    comments: true,
    terms: [
      {
        id: "desktop-context",
        re: /ZCode Desktop|Desktop Context|Desktop host/,
        sharedWith: ["protocol-zcode", "protocol-v5"],
      },
      {
        id: "gen-ui",
        re: /gen[-_ ]ui|GEN_UI|genUi|GenUi|Gen ?UI/,
        sharedWith: ["protocol-zcode", "protocol-v5"],
      },
      { id: "visualize", re: /visuali[sz]/i, sharedWith: ["protocol-zcode", "protocol-v5"] },
      {
        id: "code-comment",
        re: /code[-_]comment|CodeComment|Inline Code Comments/,
        sharedWith: ["protocol-zcode", "protocol-v5"],
      },
      {
        id: "file-links",
        re: /file[-_]links|FileLinks/,
        sharedWith: ["protocol-zcode", "protocol-v5"],
      },
      { id: "plugin-link", re: /plugin:\/\//, sharedWith: ["protocol-zcode", "protocol-v5"] },
    ],
    methods: [],
  },
  "stream-json": {
    crates: ["headless"],
    comments: false,
    terms: [{ id: "stream-json", re: /stream[-_]?json/i }],
    methods: [],
  },
};

/// 词汇的例外：只按文件与精确片段（照 export/scan.mjs 的写法），没用到的是死项。
export const PROTOCOL_TERM_EXCEPTIONS = [
  // APP-CTX（context.md §15.5）：页面状态的 attachment 来源名是存储格式（与 Node 记录同名），角色表按它把这种材料留在 user 侧，
  // 冷读的重放也按它；内容（文字、写法）在 App 前端层。
  {
    file: "crates/kernel-api/src/input_extras.rs",
    fragment: 'pub const PAGE_STATE_SOURCE: &str = "gen_ui_state";',
    why: "存储格式里的来源名（APP-CTX）",
  },
  // A0 盘点时已有（2026-10-25）：会话 API 的模块说明提到第二阶段的协议前端。只是注释，不改行为；改不改由用户定
  // （app-protocol.md §9.6）。
  {
    file: "crates/kernel-api/src/session_api.rs",
    fragment: "第二阶段的 ZCode Protocol 前端也一样",
    why: "已有的模块说明（A0 盘点），待用户决定",
  },
  {
    file: "crates/kernel-api/src/session_api.rs",
    fragment: "列出的 V4 适配层输入",
    why: "已有的模块说明（A0 盘点），待用户决定",
  },
];
