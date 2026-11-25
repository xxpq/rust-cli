// 词表与构造的规则表（工作线 F126）：枢纽 DTO 只经 kernel-api 的构造函数建（boundaries/constructors.mjs）；内置工具名只在
// kernel-api 的 `tool_names`（literals.mjs 的一条规则）。policy.mjs 重新导出这里的表，check-boundaries.mjs 照旧只读 policy。

/// 枢纽 DTO（architecture-review.md S0）：`owner` 以外的 crate 不用完整的结构体字面量构造它们。`hint` 是该用的构造函数；
/// `skipCrates` 里的 crate 有同名的别的类型（执行协议的镜像 DTO），不查。
export const DTO_CONSTRUCTORS = {
  owner: "kernel-api",
  types: [
    { name: "ModelSelection", hint: "ModelSelection::new(provider, model)" },
    {
      name: "ContextRequest",
      hint: "ContextRequest::new(session, workspace, source, mode, selection)",
    },
    {
      name: "QueryRequest",
      hint: "QueryRequest::new(session, run, workspace, messages, selection, mode, source)",
    },
    { name: "SessionView", hint: "SessionView::new(meta, messages, selection, mode)" },
    {
      name: "ModelCall",
      hint: "ModelCall::new(message, source, selection, started_at_ms, completed_at_ms, status)",
    },
    { name: "BackgroundEntry", hint: "BackgroundEntry::new(id, kind, description, status)" },
    { name: "HookEvent", hint: "HookEvent::new(name, session, workspace, payload)" },
    {
      name: "OperationContext",
      hint: "OperationContext::new(environment, generation, operation, origin)",
      skipCrates: ["exec-wire", "exec-client", "exec-server", "env-remote"],
    },
    {
      name: "McpTransportConfig::Stdio",
      hint: "McpTransportConfig::stdio(command, args)",
      defaults: {
        env: ["Default::default()", "BTreeMap::new()", "std::collections::BTreeMap::new()"],
        cwd: ["None"],
      },
    },
    { name: "McpTransportConfig::Sse", hint: "McpTransportConfig::sse(url, headers)" },
    { name: "McpTransportConfig::Http", hint: "McpTransportConfig::http(url, headers)" },
    {
      name: "ToolUseContext",
      hint: "rust_cli_kernel_api::testing::tool_use_context() (tests; the execution pipeline builds it)",
    },
    // 工作线 F137：执行协议有同名的镜像 DTO 与 `Request::StartCommand` 变体的 crate 不查（同 OperationContext）。
    {
      name: "StartCommand",
      hint: "StartCommand::new(command, cwd, task)",
      skipCrates: ["exec-wire", "exec-client", "exec-server", "env-remote"],
    },
    { name: "RunView", hint: "RunView::new(mode, workspace)" },
    // 工作线 F137：只批准这一次（`once: true`）只有判决链给出，照写字面量。
    {
      name: "PermissionDecision::Ask",
      hint: "PermissionDecision::ask(message, suggestions)",
      defaults: { once: ["false"] },
    },
  ],
};

/// 例外（按文件）：`{ files, types, reason }`。
export const DTO_CONSTRUCTOR_EXCEPTIONS = [
  {
    files: [
      "crates/query/src/executor.rs",
      "crates/session/src/slash.rs",
      "crates/compact/src/restore.rs",
    ],
    types: ["ToolUseContext"],
    reason:
      "执行管线（主循环、斜杠命令、压缩后重读文件）装出一次工具调用的上下文：12 个字段都来自调用方自己的状态，没有缺省值可省；穷举字面量让加字段时这里编译失败",
  },
  {
    files: ["crates/exec-server/src/convert_in.rs"],
    types: ["McpTransportConfig::Sse", "McpTransportConfig::Http"],
    reason: "执行协议的 MCP 传输换算成内核的配置：逐字段对应（同 X1 的穷举换算）",
  },
];

/// 内置工具名（README §4.1 第 8 条）：产品代码里整个字面量是这些名字时，只许在 kernel-api 的 `tool_names.rs`。是
/// literals.mjs 的一条规则（`VOLATILE_LITERALS` 的写法，policy.mjs 把它接在后面）；测试不查。旧名（`Task`、`KillShell` 等）
/// 不在里面。
export const TOOL_NAME_LITERAL = {
  name: "built-in tool name",
  pattern: new RegExp(
    `^(?:${[
      "Agent",
      "AskUserQuestion",
      "Bash",
      "CronCreate",
      "CronDelete",
      "CronList",
      "Edit",
      "EnterPlanMode",
      "EnterWorktree",
      "ExitPlanMode",
      "ExitWorktree",
      "Glob",
      "Grep",
      "ListAgents",
      "ListMcpResourcesTool",
      "Monitor",
      "MultiEdit",
      "NotebookEdit",
      "PowerShell",
      "Read",
      "ReadMcpResourceTool",
      "ReadMcpResourceDirTool",
      "ReportFindings",
      "ScheduleWakeup",
      "SendMessage",
      "SendUserMessage",
      "Skill",
      "TaskCreate",
      "TaskGet",
      "TaskList",
      "TaskOutput",
      "TaskStop",
      "TaskUpdate",
      "TodoWrite",
      "ToolSearch",
      "WaitForMcpServers",
      "WebFetch",
      "WebSearch",
      "Write",
    ].join("|")})$`,
  ),
  owners: ["crates/kernel-api/src/tool_names.rs"],
  tests: false,
  hint: "use rust_cli_kernel_api::tool_names",
};

/// 内置工具名的例外（同 `VOLATILE_EXCEPTIONS` 的写法，policy.mjs 把它接在后面）。
export const TOOL_NAME_EXCEPTIONS = [
  {
    files: ["crates/env-local/src/host.rs"],
    rules: ["built-in tool name"],
    reason: "Windows 上 shell 的名字 PowerShell（环境事实），不是工具名",
  },
];

// ── 厂商错误词（provider-errors.md §5）。检查在 boundaries/vendor.mjs；policy.mjs 重新导出。──

/// 厂商与协议的错误词。`owners` 是能出现的位置：以 `/` 结尾的是目录（含其中的测试文件）；`.rs` 文件连同它的测试文件
/// （同名加 `_` 开头、只在测试里编译的文件，如 `zhipu_tests.rs`）。
/// 测试文件（isTestFile 与 `_tests_*.rs`、`test_support.rs`、`test_more.rs`，含 crate 的 `tests/` 目录）不查 `values`、`codes`
/// 与 `protocol`：测试要把厂商的原始正文端到端喂进去。`selector` 连测试文件也查。
export const VENDOR_ERROR_TERMS = {
  vendors: [
    {
      // 智谱（z.ai、bigmodel.cn，含 ZCode 套餐网关）：码表与结束原因只在它自己的分类器（provider-errors.md §5.3）。
      id: "zhipu",
      owners: ["crates/model/src/errors/zhipu.rs"],
      // 智谱专有的结束原因（字符串字面量与之完全相同才算）：`sensitive` 是内容拦截，`network_error` 是流内的网络失败。
      values: ["sensitive", "network_error"],
      // 业务码（字符串字面量与之完全相同才算）。与 errors/zhipu.rs 的 CODES 相同，只去掉 `500`（与 HTTP 状态同名，太常见）；
      // vendor.test.mjs 比对两边，防止漂移。
      codes: [
        "1005",
        "1006",
        "1008",
        "1113",
        "1120",
        "1210",
        "1211",
        "1213",
        "1214",
        "1230",
        "1234",
        "1261",
        "1301",
        "1302",
        "1303",
        "1304",
        "1305",
        "1308",
        "1309",
        "1310",
        "1311",
        "1312",
        "1313",
        "1314",
        "1315",
        "1316",
        "1317",
        "1318",
        "1319",
        "1320",
        "1321",
        "2007",
        "2056",
        "20097",
        "3001",
        "3002",
        "3006",
        "3007",
        "3008",
        "3009",
        "3010",
      ],
      // 代码里的数字字面量（去掉注释与字符串之后的 `1302`、`1302u16`、`1_302`）也算码的 crate：处理模型错误的 crate。
      // net、env-local 里 WebSocket 的关闭码 1005 / 1006 是另一回事，不在其中。
      numericCrates: [
        "model",
        "query",
        "session",
        "compact",
        "agents",
        "headless",
        "tui",
        "runtime",
        "kernel-api",
        "protocol-zcode",
      ],
      // 选厂商的写法 `Vendor::Zhipu`（含 `Vendor::{…, Zhipu}`，只看代码）：只在分类器目录与按端点选厂商的 profiles.rs
      // （provider-errors.md §2.2，方案表是唯一来源）。测试文件也查。
      selector: {
        re: /\bVendor\s*::\s*(?:Zhipu\b|\{[^}]*\bZhipu\b)/,
        owners: ["crates/model/src/errors/", "crates/model/src/profiles.rs"],
      },
    },
  ],
  // Anthropic / OpenAI 协议的错误类型、错误码与结束原因（字符串字面量与之完全相同才算；`content-filter` 这类带连字符的
  // 别的写法不算）：只在分类器目录（provider-errors.md §5.1、§5.2、§5.4）。
  protocol: {
    owners: ["crates/model/src/errors/"],
    groups: [
      // 配额与账单用尽（中性分类 QuotaExhausted）。
      {
        id: "quota",
        values: [
          "insufficient_quota",
          "billing_error",
          "credit_balance_exhausted",
          "organization_spend_limit_exceeded",
          "project_spend_limit_exceeded",
          "organization_usage_limit_exceeded",
          "exceeded_current_quota_error",
        ],
      },
      // 内容拦截（ContentBlocked；OpenAI 的结束原因）。
      { id: "content-filter", values: ["content_filter"] },
      // 过载（Overloaded）。
      {
        id: "overloaded",
        values: ["overloaded_error", "server_is_overloaded", "engine_overloaded_error"],
      },
      // 限流（RateLimited）。
      {
        id: "rate-limit",
        values: ["rate_limit_error", "rate_limit_exceeded", "rate_limit_reached_error"],
      },
      // 请求过大（TooLarge）。
      { id: "too-large", values: ["request_too_large"] },
      // 上下文超长（ContextOverflow；Anthropic 的结束原因与 OpenAI 的错误码）。
      {
        id: "context-overflow",
        values: ["model_context_window_exceeded", "context_length_exceeded"],
      },
    ],
  },
};
